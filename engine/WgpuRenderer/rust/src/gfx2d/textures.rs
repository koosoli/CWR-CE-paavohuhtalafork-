use slotmap::{Key, KeyData, SlotMap};

slotmap::new_key_type! {
    struct TexKey;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TexFormat {
    Rgba8,
    Bc1,
    Bc2,
    Bc3,
}

impl TexFormat {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(TexFormat::Rgba8),
            1 => Some(TexFormat::Bc1),
            2 => Some(TexFormat::Bc2),
            3 => Some(TexFormat::Bc3),
            _ => None,
        }
    }

    fn wgpu_format(self) -> wgpu::TextureFormat {
        match self {
            TexFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
            TexFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
            TexFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnorm,
            TexFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
        }
    }

    fn is_block_compressed(self) -> bool {
        !matches!(self, TexFormat::Rgba8)
    }

    fn bytes_per_row(self, width: u32) -> u32 {
        match self {
            TexFormat::Rgba8 => width * 4,
            TexFormat::Bc1 => width.div_ceil(4) * 8,
            TexFormat::Bc2 | TexFormat::Bc3 => width.div_ceil(4) * 16,
        }
    }

    fn rows(self, height: u32) -> u32 {
        if self.is_block_compressed() {
            height.div_ceil(4)
        } else {
            height
        }
    }

    fn expected_len(self, width: u32, height: u32) -> u32 {
        self.bytes_per_row(width) * self.rows(height)
    }
}

pub struct TextureData<'a> {
    pub width: u32,
    pub height: u32,
    pub format: TexFormat,
    pub bytes: &'a [u8],
}

pub struct Texture2D {
    #[allow(dead_code)] // kept alive: the bind group references its view
    texture: wgpu::Texture,
    pub bind_group: wgpu::BindGroup,
}

// Handle 0 is reserved for the white fallback; slotmap versions start at 1, so a
// real key never encodes to 0.
pub struct TextureRegistry {
    map: SlotMap<TexKey, Texture2D>,
    bc_supported: bool,
}

impl TextureRegistry {
    pub fn new(bc_supported: bool) -> Self {
        TextureRegistry { map: SlotMap::with_key(), bc_supported }
    }

    pub fn get(&self, handle: u64) -> Option<&Texture2D> {
        if handle == 0 {
            return None;
        }
        self.map.get(KeyData::from_ffi(handle).into())
    }

    pub fn destroy(&mut self, handle: u64) {
        if handle != 0 {
            self.map.remove(KeyData::from_ffi(handle).into());
        }
    }

    pub fn create(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        tex: &TextureData,
    ) -> u64 {
        let TextureData { width, height, format, bytes } = *tex;
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
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
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
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }],
        });

        let key = self.map.insert(Texture2D { texture, bind_group });
        key.data().as_ffi()
    }

    pub fn update_rgba(&mut self, queue: &wgpu::Queue, handle: u64, data: &[u8]) {
        if handle == 0 {
            return;
        }
        if let Some(t) = self.map.get(KeyData::from_ffi(handle).into()) {
            let size = t.texture.size();
            if (data.len() as u32) >= size.width * size.height * 4 {
                write_pixels(queue, &t.texture, size.width, size.height, TexFormat::Rgba8, data);
            }
        }
    }
}

fn write_pixels(queue: &wgpu::Queue, texture: &wgpu::Texture, width: u32, height: u32, format: TexFormat, data: &[u8]) {
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
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
}
