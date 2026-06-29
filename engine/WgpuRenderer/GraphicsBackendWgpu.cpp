#include "EngineWgpu.hpp"

#include <Poseidon/Graphics/GraphicsEngineFactory.hpp>

namespace Poseidon
{
namespace
{
Engine* CreateWgpuBackend(const GraphicsEngineParams& params)
{
    return CreateEngineWgpu(params);
}

bool IsWgpuAvailable()
{
    // Real availability is decided at create time (device/surface bring-up).
    return true;
}
} // namespace

void RegisterWgpuGraphicsBackend()
{
    GraphicsEngineFactory::Register(GraphicsBackendDescriptor{
        "wgpu",
        "WGPU (Rust / wgpu)",
        50,
        &CreateWgpuBackend,
        &IsWgpuAvailable,
    });
}
} // namespace Poseidon
