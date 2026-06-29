#pragma once

#include "TextureWgpu.hpp"

#include <Poseidon/Graphics/Textures/TextureBank.hpp>
#include <Poseidon/Foundation/Containers/Array.hpp>

struct WgrRenderer;

namespace Poseidon
{

class TextureBankWgpu : public AbstractTextBank
{
  private:
    LLinkArray<TextureWgpu> _texture;
    WgrRenderer* _renderer;

  public:
    explicit TextureBankWgpu(WgrRenderer* renderer);
    ~TextureBankWgpu() override;

    WgrRenderer* Renderer() const { return _renderer; }
    void Detach() { _renderer = nullptr; }

    int Find(RStringB name) const;
    Ref<Texture> Load(RStringB name) override;
    Ref<Texture> LoadInterpolated(RStringB, RStringB, float) override { return nullptr; }
    MipInfo UseMipmap(Texture* tex, int level, int top) override;

    void Compact() override {}
    void Preload() override {}

    int NTextures() const override { return _texture.Size(); }
    Texture* GetTexture(int i) const override { return _texture[i]; }

    void FlushTextures() override {}
    void ReleaseAllTextures() override { _texture.Clear(); }
    void FlushBank(QFBank*) override {}

    Texture* CreateDynamic(int w, int h, const void* rgba, uint32_t size, bool mipmap = false) override;
    void UpdateDynamic(Texture* tex, const void* rgba, uint32_t size) override;
};

} // namespace Poseidon
