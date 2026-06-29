#include "TextureWgpu.hpp"
#include "TextureBankWgpu.hpp"

#include <Poseidon/Graphics/Core/MipmapLayout.hpp>
#include <Poseidon/Graphics/Textures/PAADecoder.hpp>
#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/IO/Streams/QBStream.hpp>
#include <Poseidon/IO/Streams/QStream.hpp>

#include <cstring>
#include <vector>

namespace Poseidon
{

namespace
{

bool IsPaaName(const char* name)
{
    const char* ext = name ? strrchr(name, '.') : nullptr;
    return ext && strcmpi(ext, ".paa") == 0;
}

PacFormat BasicFormat(const char* name)
{
    return IsPaaName(name) ? PacARGB4444 : PacARGB1555;
}

PacFormat DstFormat(PacFormat srcFormat)
{
    switch (srcFormat)
    {
        case PacP8:
            return PacARGB1555;
        default:
            return srcFormat;
    }
}

int BcFormatFor(PacFormat fmt)
{
    switch (fmt)
    {
        case PacDXT1:
            return WGR_TEX_BC1;
        case PacDXT2:
        case PacDXT3:
            return WGR_TEX_BC2;
        case PacDXT4:
        case PacDXT5:
            return WGR_TEX_BC3;
        default:
            return -1;
    }
}

} // namespace

TextureWgpu::TextureWgpu(TextureBankWgpu* bank) : _bank(bank) {}

TextureWgpu::~TextureWgpu()
{
    if (_gpuHandle && _bank)
    {
        if (WgrRenderer* r = _bank->Renderer())
        {
            wgr_texture_destroy(r, _gpuHandle);
        }
    }
}

int TextureWgpu::Init()
{
    PacFormat format = BasicFormat(Name());

    ITextureSourceFactory* factory = SelectTextureSourceFactory(Name());
    if (!factory || !factory->Check(Name()))
    {
        _nMipmaps = 0;
        return -1;
    }
    _src = factory->Create(Name(), _mipmaps, MAX_MIPMAPS);
    if (!_src)
    {
        return -1;
    }

    format = _src->GetFormat();
    if (format == PacARGB4444 || format == PacAI88 || format == PacARGB8888)
    {
        _src->ForceAlpha();
    }

    const PacFormat dFormat = DstFormat(format);

    const int nMipmaps = _src->GetMipmapCount();
    int i = 0;
    for (; i < nMipmaps; i++)
    {
        PacLevelMem& mip = _mipmaps[i];
        mip.SetDestFormat(dFormat, 8);
        if (mip._w < 2 || mip._h < 2)
        {
            break;
        }
    }
    _nMipmaps = i;

    _w = _mipmaps[0]._w;
    _h = _mipmaps[0]._h;
    return 0;
}

void TextureWgpu::InitDynamic(int w, int h, const void* rgba, uint32_t size)
{
    _dynamic = true;
    _uploadTried = true;
    _w = w;
    _h = h;
    _nMipmaps = 1;
    if (WgrRenderer* r = _bank ? _bank->Renderer() : nullptr)
    {
        _gpuHandle = wgr_texture_create(r, static_cast<uint32_t>(w), static_cast<uint32_t>(h), WGR_TEX_RGBA8,
                                        static_cast<const uint8_t*>(rgba), size);
    }
    if (!_gpuHandle)
    {
        LOG_WARN(Graphics, "Wgpu: failed to upload dynamic texture {}x{} size={}", w, h, size);
    }
}

void TextureWgpu::UpdateDynamic(const void* rgba, uint32_t size)
{
    if (!_gpuHandle) {
        return;
    }

    if (WgrRenderer* r = _bank ? _bank->Renderer() : nullptr)
    {
        wgr_texture_update(r, _gpuHandle, static_cast<const uint8_t*>(rgba), size);
    }
}

uint64_t TextureWgpu::EnsureUploaded()
{
    if (_gpuHandle || _uploadTried)
    {
        return _gpuHandle;
    }
    _uploadTried = true;

    WgrRenderer* r = _bank ? _bank->Renderer() : nullptr;
    if (!r || _w <= 0 || _h <= 0)
    {
        return 0;
    }

    const PacFormat dst = _nMipmaps > 0 ? _mipmaps[0].DstFormat() : PacFormatN;
    const int bcFormat = BcFormatFor(dst);
    if (bcFormat >= 0 && _src)
    {
        const auto layout = render::mipmap::ComputeLayout(dst, _w, _h);
        std::vector<uint8_t> blocks(static_cast<size_t>(layout.dataSize));
        if (_src->GetMipmapData(blocks.data(), _mipmaps[0], 0))
        {
            _gpuHandle = wgr_texture_create(r, static_cast<uint32_t>(_w), static_cast<uint32_t>(_h), bcFormat,
                                            blocks.data(), static_cast<uint32_t>(blocks.size()));
        }
    }

    // Fallback (non-DXT formats, or a failed block upload): decode the whole file
    // to RGBA8 via the shared PAA decoder and upload that.
    if (!_gpuHandle)
    {
        QIFStreamB stream;
        stream.AutoOpen(Name());
        const IFileBuffer* fb = stream.GetBuffer();
        if (fb && !fb->GetError() && fb->GetSize() > 0)
        {
            DecodedImage img =
                DecodePAABuffer(fb->GetData(), static_cast<size_t>(fb->GetSize()), IsPaaName(Name()));
            if (img.valid())
            {
                _gpuHandle = wgr_texture_create(r, static_cast<uint32_t>(img.width), static_cast<uint32_t>(img.height),
                                                WGR_TEX_RGBA8, img.rgba.data(), static_cast<uint32_t>(img.rgba.size()));
                _w = img.width;
                _h = img.height;
            }
        }
    }

    if (!_gpuHandle)
    {
        LOG_WARN(Graphics, "Wgpu: failed to upload texture {}", Name());
    }
    return _gpuHandle;
}

} // namespace Poseidon
