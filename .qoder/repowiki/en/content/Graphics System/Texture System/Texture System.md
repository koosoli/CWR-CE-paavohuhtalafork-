# Texture System

<cite>
**Referenced Files in This Document**
- [TextureBankGL33_Core.cpp](file://engine/PoseidonGL33/TextureBankGL33_Core.cpp)
- [TextureBankGL33_Cache.cpp](file://engine/PoseidonGL33/TextureBankGL33_Cache.cpp)
- [TextureGL33_Init.cpp](file://engine/PoseidonGL33/TextureGL33_Init.cpp)
- [TextureGL33_Loading.cpp](file://engine/PoseidonGL33/TextureGL33_Loading.cpp)
- [TextureGL33.hpp](file://engine/PoseidonGL33/TextureGL33.hpp)
- [GraphicsBackendGL33.cpp](file://engine/PoseidonGL33/GraphicsBackendGL33.cpp)
- [TextureBankWgpu.cpp](file://engine/WgpuRenderer/TextureBankWgpu.cpp)
- [TextureBankWgpu.hpp](file://engine/WgpuRenderer/TextureBankWgpu.hpp)
- [TextureWgpu.cpp](file://engine/WgpuRenderer/TextureWgpu.cpp)
- [TextureWgpu.hpp](file://engine/WgpuRenderer/TextureWgpu.hpp)
- [EngineGL33_Material.cpp](file://engine/PoseidonGL33/EngineGL33_Material.cpp)
- [EngineWgpu.cpp](file://engine/WgpuRenderer/EngineWgpu.cpp)
- [fuzz_paa.cpp](file://apps/fuzzers/Fuzzer/fuzz_paa.cpp)
</cite>

## Table of Contents
1. [Introduction](#introduction)
2. [Project Structure](#project-structure)
3. [Core Components](#core-components)
4. [Architecture Overview](#architecture-overview)
5. [Detailed Component Analysis](#detailed-component-analysis)
6. [Dependency Analysis](#dependency-analysis)
7. [Performance Considerations](#performance-considerations)
8. [Troubleshooting Guide](#troubleshooting-guide)
9. [Conclusion](#conclusion)
10. [Appendices](#appendices)

## Introduction
This document explains the texture management system, focusing on image loading, format conversion, GPU texture creation, and backend integration. It covers the TextureBank architecture, caching strategies, memory optimization techniques, supported formats (PAA, DDS, PNG, JPEG), mipmapping, compression, and format detection. It also details how OpenGL and WGPU backends are used for upload and management, and provides guidance for adding new formats and optimizing performance. Finally, it addresses texture streaming, LOD management, and memory budget considerations.

## Project Structure
The texture system is split into:
- Backend-agnostic interfaces and shared logic
- OpenGL-specific implementation (PoseidonGL33)
- WGPU-specific implementation (WgpuRenderer)
- Format support via decoders and fuzz tests

```mermaid
graph TB
subgraph "OpenGL Backend"
GL_TextureBank["TextureBankGL33"]
GL_Texture["TextureGL33"]
GL_Backend["GraphicsBackendGL33"]
end
subgraph "WGPU Backend"
WG_TextureBank["TextureBankWgpu"]
WG_Texture["TextureWgpu"]
WG_Engine["EngineWgpu"]
end
subgraph "Format Support"
PAA["PAA Decoder"]
DDS["DDS Loader"]
PNG["PNG Loader"]
JPG["JPEG Loader"]
end
GL_TextureBank --> GL_Texture
GL_Backend --> GL_TextureBank
WG_TextureBank --> WG_Texture
WG_Engine --> WG_TextureBank
PAA --> GL_TextureBank
DDS --> GL_TextureBank
PNG --> GL_TextureBank
JPG --> GL_TextureBank
PAA --> WG_TextureBank
DDS --> WG_TextureBank
PNG --> WG_TextureBank
JPG --> WG_TextureBank
```

[No sources needed since this diagram shows conceptual workflow, not actual code structure]

## Core Components
- TextureBank: Central registry that owns textures, manages caching, and coordinates uploads to the GPU.
- Texture (backend-specific): Encapsulates GPU-side resources and metadata (format, size, flags).
- Image loaders: Decode various formats into a common pixel buffer with known internal format and dimensions.
- Backend integrations: Upload pixel data to GPU textures and manage lifecycle.

Key responsibilities:
- Detecting and normalizing image formats
- Generating mipmaps when requested
- Caching decoded or compressed textures to avoid redundant work
- Managing memory budgets and eviction policies
- Exposing stable handles to rendering systems

**Section sources**
- [TextureBankGL33_Core.cpp](file://engine/PoseidonGL33/TextureBankGL33_Core.cpp)
- [TextureBankGL33_Cache.cpp](file://engine/PoseidonGL33/TextureBankGL33_Cache.cpp)
- [TextureGL33.hpp](file://engine/PoseidonGL33/TextureGL33.hpp)
- [TextureBankWgpu.hpp](file://engine/WgpuRenderer/TextureBankWgpu.hpp)
- [TextureWgpu.hpp](file://engine/WgpuRenderer/TextureWgpu.hpp)

## Architecture Overview
The TextureBank abstracts over multiple backends while providing a unified API for creating and managing textures. Each backend implements its own Texture class and upload path.

```mermaid
classDiagram
class TextureBank {
+createTexture(params) TextureHandle
+getTexture(handle) Texture*
+releaseTexture(handle) void
+cachePolicy() CacheConfig
}
class TextureGL33 {
+id GLuint
+width uint32
+height uint32
+format InternalFormat
+mipLevels uint32
+flags TextureFlags
+uploadPixels(data, size) void
+generateMipmaps() void
}
class TextureWgpu {
+resource wgpu : : Texture
+width uint32
+height uint32
+format TextureFormat
+mipLevels uint32
+flags TextureFlags
+uploadPixels(data, size) void
+generateMipmaps() void
}
class ImageLoader {
+load(path) ImageBuffer
+detectFormat(path) Format
}
TextureBank --> TextureGL33 : "creates (OpenGL)"
TextureBank --> TextureWgpu : "creates (WGPU)"
TextureBank --> ImageLoader : "uses"
```

**Diagram sources**
- [TextureBankGL33_Core.cpp](file://engine/PoseidonGL33/TextureBankGL33_Core.cpp)
- [TextureGL33.hpp](file://engine/PoseidonGL33/TextureGL33.hpp)
- [TextureBankWgpu.hpp](file://engine/WgpuRenderer/TextureBankWgpu.hpp)
- [TextureWgpu.hpp](file://engine/WgpuRenderer/TextureWgpu.hpp)

**Section sources**
- [GraphicsBackendGL33.cpp](file://engine/PoseidonGL33/GraphicsBackendGL33.cpp)
- [EngineWgpu.cpp](file://engine/WgpuRenderer/EngineWgpu.cpp)

## Detailed Component Analysis

### OpenGL Texture Bank and Texture
- TextureBankGL33 maintains a cache keyed by asset identifiers and parameters (size, format, flags).
- TextureGL33 wraps an OpenGL texture object with metadata and methods for uploading pixels and generating mipmaps.
- Loading pipeline detects format, decodes to a CPU buffer, optionally compresses to a GPU-friendly format, then uploads.

```mermaid
sequenceDiagram
participant App as "Application"
participant GLTB as "TextureBankGL33"
participant Loader as "ImageLoader"
participant GLTex as "TextureGL33"
participant GL as "OpenGL"
App->>GLTB : createTexture("path", params)
GLTB->>GLTB : lookupCache(key)
alt cached
GLTB-->>App : handle
else not cached
GLTB->>Loader : load("path")
Loader-->>GLTB : ImageBuffer(format, width, height, data)
GLTB->>GLTex : allocate(width, height, format, mipLevels)
GLTex->>GL : glGenTextures()
GLTB->>GLTex : uploadPixels(data)
GLTex->>GL : glTexImage2D(...)
opt generateMipmaps
GLTex->>GL : glGenerateMipmap(...)
end
GLTB->>GLTB : insertCache(key, handle)
GLTB-->>App : handle
end
```

**Diagram sources**
- [TextureBankGL33_Core.cpp](file://engine/PoseidonGL33/TextureBankGL33_Core.cpp)
- [TextureBankGL33_Cache.cpp](file://engine/PoseidonGL33/TextureBankGL33_Cache.cpp)
- [TextureGL33_Init.cpp](file://engine/PoseidonGL33/TextureGL33_Init.cpp)
- [TextureGL33_Loading.cpp](file://engine/PoseidonGL33/TextureGL33_Loading.cpp)

**Section sources**
- [TextureBankGL33_Core.cpp](file://engine/PoseidonGL33/TextureBankGL33_Core.cpp)
- [TextureBankGL33_Cache.cpp](file://engine/PoseidonGL33/TextureBankGL33_Cache.cpp)
- [TextureGL33_Init.cpp](file://engine/PoseidonGL33/TextureGL33_Init.cpp)
- [TextureGL33_Loading.cpp](file://engine/PoseidonGL33/TextureGL33_Loading.cpp)
- [TextureGL33.hpp](file://engine/PoseidonGL33/TextureGL33.hpp)

### WGPU Texture Bank and Texture
- TextureBankWgpu mirrors the OpenGL bank’s responsibilities but uses WGPU APIs for resource creation and uploads.
- TextureWgpu encapsulates a WGPU texture and associated views, handling pixel uploads and mipmap generation.

```mermaid
sequenceDiagram
participant App as "Application"
participant WGTLB as "TextureBankWgpu"
participant Loader as "ImageLoader"
participant WGTex as "TextureWgpu"
participant WGPU as "WGPU"
App->>WGTLB : createTexture("path", params)
WGTLB->>WGTLB : lookupCache(key)
alt cached
WGTLB-->>App : handle
else not cached
WGTLB->>Loader : load("path")
Loader-->>WGTLB : ImageBuffer(format, width, height, data)
WGTLB->>WGTex : allocate(width, height, format, mipLevels)
WGTex->>WGPU : wgpuDeviceCreateTexture(...)
WGTLB->>WGTex : uploadPixels(data)
WGTex->>WGPU : wgpuQueueWriteTexture(...)
opt generateMipmaps
WGTex->>WGPU : wgpuComputePass / blit mip chain
end
WGTLB->>WGTLB : insertCache(key, handle)
WGTLB-->>App : handle
end
```

**Diagram sources**
- [TextureBankWgpu.cpp](file://engine/WgpuRenderer/TextureBankWgpu.cpp)
- [TextureWgpu.cpp](file://engine/WgpuRenderer/TextureWgpu.cpp)

**Section sources**
- [TextureBankWgpu.hpp](file://engine/WgpuRenderer/TextureBankWgpu.hpp)
- [TextureBankWgpu.cpp](file://engine/WgpuRenderer/TextureBankWgpu.cpp)
- [TextureWgpu.hpp](file://engine/WgpuRenderer/TextureWgpu.hpp)
- [TextureWgpu.cpp](file://engine/WgpuRenderer/TextureWgpu.cpp)

### Image Loading and Format Detection
- Supported formats include PAA, DDS, PNG, and JPEG.
- Format detection typically relies on file headers and extensions; decoding produces a normalized RGBA or RGB buffer with explicit dimensions.
- For PAA, dedicated decoders exist and are exercised by fuzz tests.

```mermaid
flowchart TD
Start(["Load Request"]) --> Detect["Detect Format from Header/Extension"]
Detect --> IsPAA{"Is PAA?"}
IsPAA --> |Yes| DecodePAA["Decode PAA to Buffer"]
IsPAA --> |No| IsDDS{"Is DDS?"}
IsDDS --> |Yes| DecodeDDS["Decode DDS to Buffer"]
IsDDS --> |No| IsPNG{"Is PNG?"}
IsPNG --> |Yes| DecodePNG["Decode PNG to Buffer"]
IsPNG --> |No| IsJPG{"Is JPEG?"}
IsJPG --> |Yes| DecodeJPG["Decode JPEG to Buffer"]
IsJPG --> |No| Error["Unsupported Format"]
DecodePAA --> Normalize["Normalize to RGBA/RGB + Size"]
DecodeDDS --> Normalize
DecodePNG --> Normalize
DecodeJPG --> Normalize
Normalize --> Return(["Return ImageBuffer"])
Error --> Return
```

**Diagram sources**
- [fuzz_paa.cpp](file://apps/fuzzers/Fuzzer/fuzz_paa.cpp)

**Section sources**
- [fuzz_paa.cpp](file://apps/fuzzers/Fuzzer/fuzz_paa.cpp)

### Mipmapping and Texture Compression
- Mipmaps can be generated automatically after upload if requested by the caller.
- Compression targets GPU-native formats where available; otherwise, uncompressed formats are used.
- The decision between compressed vs uncompressed depends on platform capabilities and requested usage flags.

```mermaid
flowchart TD
Entry(["Upload Pixels"]) --> CheckMips{"Mipmaps Required?"}
CheckMips --> |No| Commit["Commit Texture"]
CheckMips --> |Yes| Generate["Generate Mipchain"]
Generate --> CompressCheck{"Compressed Format Available?"}
CompressCheck --> |Yes| UseCompressed["Use GPU-compressed format"]
CompressCheck --> |No| UseUncompressed["Use uncompressed format"]
UseCompressed --> Commit
UseUncompressed --> Commit
Commit --> Exit(["Done"])
```

[No sources needed since this diagram shows conceptual workflow, not actual code structure]

### Integration with Rendering Systems
- Material systems bind textures through handles provided by TextureBank.
- Backends translate these handles into native bindings (GL texture IDs or WGPU textures).

```mermaid
sequenceDiagram
participant Mat as "Material System"
participant GLTB as "TextureBankGL33"
participant GLTex as "TextureGL33"
participant GL as "OpenGL"
Mat->>GLTB : getTexture(handle)
GLTB-->>Mat : TextureGL33*
Mat->>GLTex : bind()
GLTex->>GL : glBindTexture(GL_TEXTURE_2D, id)
```

**Diagram sources**
- [EngineGL33_Material.cpp](file://engine/PoseidonGL33/EngineGL33_Material.cpp)
- [TextureGL33.hpp](file://engine/PoseidonGL33/TextureGL33.hpp)

**Section sources**
- [EngineGL33_Material.cpp](file://engine/PoseidonGL33/EngineGL33_Material.cpp)

## Dependency Analysis
- TextureBank implementations depend on:
  - Image loaders for decoding and normalization
  - Backend-specific Texture classes for GPU resource management
  - Platform capability queries for choosing optimal formats
- Rendering systems depend on TextureBank for texture acquisition and binding.

```mermaid
graph LR
App["Application"] --> GLTB["TextureBankGL33"]
App --> WGTLB["TextureBankWgpu"]
GLTB --> GLTex["TextureGL33"]
WGTLB --> WGTex["TextureWgpu"]
GLTB --> Loader["ImageLoader"]
WGTLB --> Loader
GLTB --> GL["OpenGL"]
WGTLB --> WGPU["WGPU"]
```

**Diagram sources**
- [GraphicsBackendGL33.cpp](file://engine/PoseidonGL33/GraphicsBackendGL33.cpp)
- [EngineWgpu.cpp](file://engine/WgpuRenderer/EngineWgpu.cpp)
- [TextureBankGL33_Core.cpp](file://engine/PoseidonGL33/TextureBankGL33_Core.cpp)
- [TextureBankWgpu.cpp](file://engine/WgpuRenderer/TextureBankWgpu.cpp)

**Section sources**
- [GraphicsBackendGL33.cpp](file://engine/PoseidonGL33/GraphicsBackendGL33.cpp)
- [EngineWgpu.cpp](file://engine/WgpuRenderer/EngineWgpu.cpp)

## Performance Considerations
- Caching:
  - Deduplicate identical loads using keys derived from asset paths and parameters.
  - Prefer compressed GPU formats when available to reduce bandwidth and memory.
- Mipmaps:
  - Enable mipmaps for sampled textures to improve filtering and reduce aliasing.
  - Avoid unnecessary regeneration; store mip levels once per texture.
- Memory Budget:
  - Track total VRAM usage across all textures.
  - Implement LRU or priority-based eviction for offscreen caches.
- Streaming:
  - Load only required mip levels initially; stream additional levels on demand.
  - Use asynchronous loading to avoid frame stalls.
- Decoding:
  - Reuse buffers and avoid repeated allocations.
  - Parallelize decoding for large batches where safe.

[No sources needed since this section provides general guidance]

## Troubleshooting Guide
Common issues and resolutions:
- Unsupported format errors:
  - Verify file headers and ensure decoders are registered.
  - Confirm extension mapping matches expected formats.
- Missing mipmaps causing blurry textures:
  - Ensure mip generation is enabled and the texture usage flags allow sampling.
- Out-of-memory during upload:
  - Reduce texture sizes, enable compression, or lower simultaneous load counts.
  - Monitor VRAM usage and implement eviction.
- Backend-specific failures:
  - Validate OpenGL context state and WGPU device queues.
  - Check error logs from GL calls and WGPU validation layers.

**Section sources**
- [TextureGL33_Init.cpp](file://engine/PoseidonGL33/TextureGL33_Init.cpp)
- [TextureGL33_Loading.cpp](file://engine/PoseidonGL33/TextureGL33_Loading.cpp)
- [TextureWgpu.cpp](file://engine/WgpuRenderer/TextureWgpu.cpp)

## Conclusion
The texture system provides a robust, backend-agnostic foundation for loading, converting, and uploading images to the GPU. By leveraging caching, mipmapping, and compression, it balances quality and performance across OpenGL and WGPU backends. Extending format support and optimizing loading pipelines can further enhance scalability and user experience.

[No sources needed since this section summarizes without analyzing specific files]

## Appendices

### Adding a New Texture Format
Steps to integrate a new format:
- Implement a decoder that outputs a normalized buffer (RGBA/RGB) with correct dimensions.
- Register the decoder with the image loader, including header/extension detection.
- Validate with unit tests and fuzz inputs.
- Ensure backend upload paths accept the resulting format or add conversion as needed.

[No sources needed since this section provides general guidance]

### Optimizing Texture Loading Performance
Recommendations:
- Batch decode operations and reuse buffers.
- Precompute mipchains offline for static assets.
- Use async I/O and worker threads for decoding and uploads.
- Profile VRAM usage and adjust compression choices based on hardware capabilities.

[No sources needed since this section provides general guidance]