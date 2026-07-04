use slotmap::{Key, KeyData, SlotMap};

use crate::ffi::WgrSampler2D;

slotmap::new_key_type! {
    struct TextureKey;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    Rgba8,
    Bc1,
    Bc2,
    Bc3,
}

impl TextureFormat {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(TextureFormat::Rgba8),
            1 => Some(TextureFormat::Bc1),
            2 => Some(TextureFormat::Bc2),
            3 => Some(TextureFormat::Bc3),
            _ => None,
        }
    }

    pub(crate) fn wgpu_format(self) -> wgpu::TextureFormat {
        match self {
            TextureFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
            TextureFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
            TextureFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnorm,
            TextureFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
        }
    }

    pub(crate) fn is_block_compressed(self) -> bool {
        !matches!(self, TextureFormat::Rgba8)
    }

    pub(crate) fn bytes_per_row(self, width: u32) -> u32 {
        match self {
            TextureFormat::Rgba8 => width * 4,
            TextureFormat::Bc1 => width.div_ceil(4) * 8,
            TextureFormat::Bc2 | TextureFormat::Bc3 => width.div_ceil(4) * 16,
        }
    }

    pub(crate) fn rows(self, height: u32) -> u32 {
        if self.is_block_compressed() {
            height.div_ceil(4)
        } else {
            height
        }
    }

    pub(crate) fn expected_len(self, width: u32, height: u32) -> u32 {
        self.bytes_per_row(width) * self.rows(height)
    }
}

pub struct TextureData<'a> {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub bytes: &'a [u8],
}

pub struct Texture2D {
    #[allow(dead_code)] // kept alive: the bind group references its view
    texture: wgpu::Texture,
    pub bind_group: wgpu::BindGroup,
}

// Handle 0 is reserved for the white fallback; slotmap versions start at 1, so a
// real key never encodes to 0.
struct TextureRegistry {
    map: SlotMap<TextureKey, Texture2D>,
    bc_supported: bool,
}

impl TextureRegistry {
    fn new(bc_supported: bool) -> Self {
        TextureRegistry {
            map: SlotMap::with_key(),
            bc_supported,
        }
    }

    fn get(&self, handle: u64) -> Option<&Texture2D> {
        if handle == 0 {
            return None;
        }
        self.map.get(KeyData::from_ffi(handle).into())
    }

    fn destroy(&mut self, handle: u64) {
        if handle != 0 {
            self.map.remove(KeyData::from_ffi(handle).into());
        }
    }

    fn create(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        tex: &TextureData,
    ) -> u64 {
        let TextureData {
            width,
            height,
            format,
            bytes,
        } = *tex;
        if width == 0 || height == 0 {
            return 0;
        }
        if format.is_block_compressed() && !self.bc_supported {
            return 0;
        }
        if (bytes.len() as u32) < format.expected_len(width, height) {
            return 0;
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.wgpu_format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        write_pixels(queue, &texture, width, height, format, bytes);

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_texture_bind"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });

        let key = self.map.insert(Texture2D {
            texture,
            bind_group,
        });
        key.data().as_ffi()
    }

    fn update_rgba(&mut self, queue: &wgpu::Queue, handle: u64, data: &[u8]) {
        if handle == 0 {
            return;
        }
        if let Some(t) = self.map.get(KeyData::from_ffi(handle).into()) {
            let size = t.texture.size();
            if (data.len() as u32) >= size.width * size.height * 4 {
                write_pixels(
                    queue,
                    &t.texture,
                    size.width,
                    size.height,
                    TextureFormat::Rgba8,
                    data,
                );
            }
        }
    }
}

pub struct SharedTextures {
    registry: TextureRegistry,
    pub texture_layout: wgpu::BindGroupLayout,
    pub sampler_layout: wgpu::BindGroupLayout,
    #[allow(dead_code)] // kept alive: the sampler bind groups reference these
    samplers: [wgpu::Sampler; 8],
    sampler_binds: [wgpu::BindGroup; 8],
    #[allow(dead_code)] // kept alive: white_bind references its view
    white_tex: wgpu::Texture,
    white_bind: wgpu::BindGroup,
}

impl SharedTextures {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, bc_supported: bool) -> Self {
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_texture_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });

        let sampler_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_sampler_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            }],
        });

        let samplers: [wgpu::Sampler; 8] = std::array::from_fn(|i| {
            let i = i as u32;
            let point = i & WgrSampler2D::POINT != 0;
            let wrap = |clamp: bool| {
                if clamp {
                    wgpu::AddressMode::ClampToEdge
                } else {
                    wgpu::AddressMode::Repeat
                }
            };
            let filter = if point {
                wgpu::FilterMode::Nearest
            } else {
                wgpu::FilterMode::Linear
            };
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("wgr_sampler"),
                address_mode_u: wrap(i & WgrSampler2D::CLAMP_U != 0),
                address_mode_v: wrap(i & WgrSampler2D::CLAMP_V != 0),
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            })
        });
        let sampler_binds: [wgpu::BindGroup; 8] = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wgr_sampler_bind"),
                layout: &sampler_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&samplers[i]),
                }],
            })
        });

        let white_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_white"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_pixels(
            queue,
            &white_tex,
            1,
            1,
            TextureFormat::Rgba8,
            &[0xFF, 0xFF, 0xFF, 0xFF],
        );
        let white_view = white_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let white_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_white_bind"),
            layout: &texture_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&white_view),
            }],
        });

        SharedTextures {
            registry: TextureRegistry::new(bc_supported),
            texture_layout,
            sampler_layout,
            samplers,
            sampler_binds,
            white_tex,
            white_bind,
        }
    }

    // Texture bind group for `handle`, falling back to the 1x1 white texture.
    pub fn texture_bind(&self, handle: u64) -> &wgpu::BindGroup {
        self.registry
            .get(handle)
            .map_or(&self.white_bind, |t| &t.bind_group)
    }

    // Sampler bind group for a `point<<2 | clampV<<1 | clampU` index.
    pub fn sampler_bind(&self, index: usize) -> &wgpu::BindGroup {
        self.sampler_binds
            .get(index)
            .unwrap_or(&self.sampler_binds[0])
    }

    pub fn create(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, tex: &TextureData) -> u64 {
        self.registry
            .create(device, queue, &self.texture_layout, tex)
    }

    pub fn update_rgba(&mut self, queue: &wgpu::Queue, handle: u64, data: &[u8]) {
        self.registry.update_rgba(queue, handle, data);
    }

    pub fn destroy(&mut self, handle: u64) {
        self.registry.destroy(handle);
    }
}

fn write_pixels(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    format: TextureFormat,
    data: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(format.bytes_per_row(width)),
            rows_per_image: Some(format.rows(height)),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}
