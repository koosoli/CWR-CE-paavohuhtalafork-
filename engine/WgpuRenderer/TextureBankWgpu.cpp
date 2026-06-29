#include "TextureBankWgpu.hpp"

namespace Poseidon
{

TextureBankWgpu::TextureBankWgpu(WgrRenderer* renderer) : _renderer(renderer) {}

TextureBankWgpu::~TextureBankWgpu()
{
    UnlockAllTextures();
    DeleteAllAnimated();
}

int TextureBankWgpu::Find(RStringB name) const
{
    for (int i = 0; i < _texture.Size(); i++)
    {
        TextureWgpu* texture = _texture[i];
        if (texture && texture->GetName() == name)
        {
            return i;
        }
    }
    return -1;
}

Ref<Texture> TextureBankWgpu::Load(RStringB name)
{
    int index = Find(name);
    if (index >= 0)
    {
        return (Texture*)_texture[index];
    }

    int iFree = _texture.Add();
    TextureWgpu* texture = new TextureWgpu(this);
    texture->SetName(name);
    _texture[iFree] = texture;
    texture->Init();
    return texture;
}

MipInfo TextureBankWgpu::UseMipmap(Texture* tex, int /*level*/, int /*top*/)
{
    if (auto* t = static_cast<TextureWgpu*>(tex))
    {
        t->EnsureUploaded();
    }
    return MipInfo(tex, 0);
}

Texture* TextureBankWgpu::CreateDynamic(int w, int h, const void* rgba, uint32_t size, bool /*mipmap*/)
{
    auto* texture = new TextureWgpu(this);
    texture->InitDynamic(w, h, rgba, size);
    return texture;
}

void TextureBankWgpu::UpdateDynamic(Texture* tex, const void* rgba, uint32_t size)
{
    if (auto* t = static_cast<TextureWgpu*>(tex))
    {
        t->UpdateDynamic(rgba, size);
    }
}

} // namespace Poseidon
