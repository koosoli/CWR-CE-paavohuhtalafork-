/*
 * wgpu_renderer.hpp — C++ interface to the WGPU graphics backend.
 *
 * While this file contains C++ features, the actual exported symbols are all C ABI (extern "C"),
 * which the Rust side can implement using #[no_mangle] and #[repr(C)].
 */
#ifndef WGPU_RENDERER_HPP
#define WGPU_RENDERER_HPP

#include <cstdint>

#if defined(_WIN32) && !defined(WGR_STATIC)
  #define WGR_API __declspec(dllimport)
#else
  #define WGR_API
#endif

struct WgrRenderer;

// --- Math + handle aliases ---------------------------------------------------

struct WgrVec2
{
    float x, y;
};
struct WgrVec3
{
    float x, y, z;
};
struct WgrVec4
{
    float x, y, z, w;
};
struct WgrMat4
{
    float m[16]; // column-major
};

using WgrRgba8 = uint32_t;   // packed 0xAARRGGBB (engine PackedColor order)
using WgrTexture = uint64_t; // handle from wgr_texture_create; 0 = built-in white fallback
using WgrMesh = uint64_t;    // handle from wgr_mesh_create

template <typename T>
concept ContiguousContainer = requires(const T& c) {
    c.data();
    c.size();
};

template <typename T>
struct WgrSlice
{
    const T* data = nullptr;
    uint32_t length = 0;

    WgrSlice() = default;
    WgrSlice(const T* ptr, uint32_t count) : data(ptr), length(count) {}

    template <typename Container>
        requires ContiguousContainer<Container>
    WgrSlice(const Container& c) : data(c.data()), length(static_cast<uint32_t>(c.size())) { }
};

// --- Enums -------------------------------------------------------------------

/* Selects how WgrSurfaceDesc.window / .display are interpreted. */
enum WgrPlatform : int32_t
{
    WGR_PLATFORM_WIN32 = 0,   // window = HWND,         display unused
    WGR_PLATFORM_XLIB = 1,    // window = Window (XID), display = Display*
    WGR_PLATFORM_WAYLAND = 2  // window = wl_surface*,  display = wl_display*
};

enum WgrLogLevel : int32_t
{
    WGR_LOG_TRACE = 0,
    WGR_LOG_DEBUG = 1,
    WGR_LOG_INFO = 2,
    WGR_LOG_WARN = 3,
    WGR_LOG_ERROR = 4
};

enum WgrTextureFormat : int32_t
{
    WGR_TEXTURE_RGBA8 = 0,
    WGR_TEXTURE_BC1 = 1, // DXT1
    WGR_TEXTURE_BC2 = 2, // DXT3
    WGR_TEXTURE_BC3 = 3  // DXT5
};

enum WgrBlend : uint32_t
{
    WGR_BLEND_OPAQUE = 0,
    WGR_BLEND_ALPHA = 1,
    WGR_BLEND_ADDITIVE = 2,
    WGR_BLEND_SHADOW = 3
};

/* Depth-buffer interaction for a 2D/screen batch. Plain 2D and depth-disabled
 * meshes (sky: NoZBuf) use NONE; transparent / NoZWrite meshes test but don't
 * write; opaque pre-projected meshes (the laptop) test and write. */
enum WgrDepthMode : uint32_t
{
    WGR_DEPTH_NONE = 0,      // no test, no write
    WGR_DEPTH_TEST = 1,      // test (LessEqual), no write
    WGR_DEPTH_TEST_WRITE = 2 // test (LessEqual) + write
};

/* Selects what a WgrCmd does when the frame's command stream is replayed. */
enum WgrCmdKind : uint32_t
{
    WGR_CMD_DRAW_2D = 0,       // arg = index into WgrFrame.batches (its WgrDepthMode picks depth state)
    WGR_CMD_DRAW_3D = 1,       // arg = index into WgrFrame.draws3d
    WGR_CMD_CLEAR_DEPTH = 2,   // arg unused; starts a new depth-cleared segment
    WGR_CMD_DRAW_TERRAIN = 3,  // arg = index into WgrFrame.terrain_batches
    WGR_CMD_RESOLVE = 4,       // arg unused; tonemap the HDR scene to the swapchain, then draw UI display-referred
    WGR_CMD_DRAW_WATER = 5     // arg = index into WgrFrame.water_batches
};

// --- Surface / logging -------------------------------------------------------

struct WgrSurfaceDesc
{
    WgrPlatform platform;
    void* window;
    void* display;
    uint32_t width;
    uint32_t height;
};

struct WgrLogCallbacks
{
    /* `log` may be NULL; `message` is only valid for the duration of the call. */
    void (*log)(int32_t level, const char* message, void* user);
    void* user;
};

// --- Vertices ----------------------------------------------------------------

/* One screen-space vertex. `pos.x`/`pos.y` are window pixels (origin top-left),
 * `pos.z` the depth, `rhw` the reciprocal clip-w (perspective-correct interp),
 * `fog` the fog blend factor (1 = keep colour, 0 = full fog). Plain 2D uses
 * pos.z=0, rhw=1, fog=1. `color` is packed 0xAARRGGBB. */
struct WgrVertex2D
{
    WgrVec3 pos;
    float rhw;
    float fog;
    WgrVec2 uv;
    WgrRgba8 color;
};

/* One object-space mesh vertex; matches the engine's SVertex (pos, normal, uv, conform). */
struct WgrMeshVertex
{
    WgrVec3 pos;
    WgrVec3 normal;
    WgrVec2 uv;
    /* Per-vertex terrain-conform selector (0 = rigid, 1 = ClipLandKeep, 2 = ClipLandOn),
     * read by vs_main at @location(5). Meaningful only when the draw's conform mode
     * selects the per-vertex heightmap path (individual ClipLand vegetation). */
    uint32_t conform;
};

// --- Draw records ------------------------------------------------------------

/* A contiguous run of triangle-list vertices sharing one texture + blend + depth
 * mode. `texture_id` 0 selects the built-in 1x1 white texture. */
struct WgrDraw2DBatch
{
    WgrTexture texture_id;
    uint32_t first_vertex; // index into WgrFrame.verts
    uint32_t vertex_count; // multiple of 3
    WgrBlend blend;
    uint32_t sampler; // bits: point<<2 | clampV<<1 | clampU
    WgrDepthMode depth;
};

/* Sentinel for WgrDraw3D::palette_slot: this draw is not skinned. */
#define WGR_NO_PALETTE 0xFFFFFFFFu

/* Capacity of the frame-global light store (WgrFrame::lights). Must match
 * MAX_LIGHTS in rust/src/gfx3d/mod.rs. The renderer clamps to this. */
#define WGR_MAX_LIGHTS 256

/* Bits for WgrDraw3D::flags. */
enum WgrDraw3DFlags : uint32_t
{
    /* Road / decal / footprint overlay: pull the draw toward the camera with a
     * polygon-offset (mirrors GL33's SetPolygonOffsetForDecals on OnSurface
     * routing) so it wins the depth test against the coplanar terrain. */
    WGR_DRAW3D_ON_SURFACE = 1,

    /* ZBias overlay level (1..3) in bits 8-9, for non-OnSurface geometry that the
     * engine biased via SetBias(level*5) (e.g. traffic-sign overlay faces). Gets a
     * stronger, level-scaled polygon-offset than a plain surface decal. */
    WGR_DRAW3D_ZBIAS_SHIFT = 8,
    WGR_DRAW3D_ZBIAS_MASK = 0x300
};

/* Matrices per palette block (the engine's own bone-palette cap). Each skinned
 * draw's palette occupies this many matrices in WgrFrame.palette. */
#define WGR_PALETTE_SIZE 128

/* A section [index_begin, index_begin+index_count) of `mesh`, textured with
 * `texture_id` (0 = built-in white), transformed by the camera-relative `world`
 * matrix. `camera` indexes WgrFrame.cameras. For skinned draws, `palette_slot`
 * indexes a 128-matrix block in WgrFrame.palette (world pre-multiplied in) and `world`
 * is ignored; WGR_NO_PALETTE = not skinned (use `world`). */
struct WgrDraw3D
{
    WgrMesh mesh;
    uint32_t index_begin;
    uint32_t index_count;
    WgrTexture texture_id;
    WgrMat4 world;
    WgrBlend blend;
    uint32_t sampler;
    uint32_t camera;
    uint32_t palette_slot;
    WgrDepthMode depth;
    /* Alpha-test cutout threshold in [0,1]: a fragment is discarded when its
     * sampled alpha is below this. 0 disables the test (nothing discarded).
     * Mirrors GL33's per-draw alphaRef (IsAlpha ~1/255, IsTransparent 0xC0). */
    float alpha_ref;
    uint32_t flags; // WgrDraw3DFlags
    uint32_t _pad;
    /* Per-draw material lighting, folded exactly like GL33's
     * UploadVSMaterialConstants: raw MainLight diffuse/ambient x material, with
     * the sun-enable already multiplied into the sun terms (emissive shows
     * regardless). The lit shader computes emissive + sun_ambient +
     * sun_diffuse * N.L, clamps to [0,1], then multiplies the texture. Only rgb
     * is read; the w lanes ride along for 16-byte std140 alignment. */
    WgrVec4 mat_emissive;
    WgrVec4 mat_sun_ambient;
    WgrVec4 mat_sun_diffuse;
    /* Material modulation for the frame-global point/spot lights (GL33's matDif /
     * matAmb before the per-light colour): raw material diffuse/ambient (eye
     * accommodation already in, night NOT — that rides the light colour). rgb. */
    WgrVec4 mat_light_diffuse;
    WgrVec4 mat_light_ambient;
    /* Sun-only Blinn-Phong specular highlight, folded like GL33's c18: rgb = raw
     * sun diffuse x material specular (sun-enable folded in, so 0 when the sun is
     * off), w = specular power. The lit shader adds rgb * pow(N.H, max(w,1))
     * per-fragment when w > 0; w <= 0 means the material has no highlight. */
    WgrVec4 mat_specular;
    /* Terrain-conform plane for GPU vegetation (ForestPlain).
     * When conform2.z (mode) > 0 the vertex shader displaces this draw's vertices onto
     * the ground exactly like ForestPlain::Animate's two-triangle bilinear fit, so the
     * shared forest mesh is uploaded once undeformed instead of rewritten per instance.
     * All-zero (mode 0) for every non-conformed draw. */
    WgrVec4 conform0; /* inv_land_grid, -xf, -zf, bias(=BoundingCenter().y) */
    WgrVec4 conform1; /* y00, y10, d1000, d0100 */
    WgrVec4 conform2; /* d1011, d0111, mode(0=none,1=forest), _pad */
};

/* One frame-global point or spot light, shared by every 3D draw + terrain (bound
 * as a group-0 storage buffer). Position is ABSOLUTE world space (not
 * camera-relative like the geometry) so one upload serves every camera; the
 * shader reconstructs the camera-relative offset via the frame's cam_pos.
 * Colours are pre-scaled by the sun's NightEffect on the CPU (fade out by day,
 * matching GL33's night-only local lights). Mirrors GL33's per-draw VS lights. */
struct WgrLight
{
    WgrVec4 pos;     /* xyz = world-absolute position, w = start-attenuation distance */
    WgrVec4 diffuse; /* rgb = diffuse * nightEffect */
    WgrVec4 ambient; /* rgb = ambient * nightEffect */
    WgrVec4 dir;     /* xyz = beam direction (spot), w = isSpot (1) else 0 */
};

/* --- GPU-driven retained scene (docs/gpu-culling-and-depth-plan.md Stage 3b) ---
 *
 * C++ registers each opaque-rigid LODShapeWithShadow once (its LODs + per-section
 * geometry and material) via wgr_model_register, then streams instances: static
 * clutter as add/update/remove slots, dynamics re-copied each frame via
 * wgr_set_dynamic. The GPU cull compute walks the retained instances each frame and
 * emits indirect draws, so the CPU stops walking these objects per frame. Layouts
 * mirror the Rust #[repr(C)] structs in rust/src/ffi.rs (size-asserted both sides). */

/* One drawable section of a model LOD. `mesh` + the index range address the shared
 * geometry pool (resolved to base_vertex/first_index at registration); `variant`
 * selects the pipeline-variant partition (0 = solid, 1 = alpha-cutout). */
struct WgrModelSection
{
    WgrMesh mesh;
    uint32_t index_begin;
    uint32_t index_count;
    uint32_t variant;
    uint32_t _pad;
};

/* Per-section shading, parallel to a model's sections (one per section). The RAW
 * material is folded with the frame sun in the GPU-driven fragment shader (matching
 * the per-draw path); `texture_id` is a wgr_texture_create handle, resolved to a
 * bindless slot at registration. */
struct WgrModelMaterial
{
    WgrVec4 emissive;
    WgrVec4 ambient;
    WgrVec4 diffuse;
    WgrVec4 specular; /* w = specular power */
    WgrTexture texture_id;
    uint32_t sampler;
    float alpha_ref;
};

/* One drawable LOD level: its FindSqrtLevel resolution threshold (_resolutions[i]) +
 * the range of sections it draws (`section_base` is RELATIVE to this model's
 * sections). */
struct WgrModelLod
{
    float resolution;
    uint32_t section_base;
    uint32_t section_count;
    uint32_t is_decal;
};

/* One retained instance. Layout matches the GPU-side InstanceGpu exactly: `world` is
 * the ABSOLUTE model->world transform (the GPU-driven VS subtracts cam_pos),
 * `center.xyz` the world bounding-sphere center + `center.w` the uniform scale (both
 * read by the cull compute), `model` the wgr_model_register id. */
struct WgrInstance
{
    WgrMat4 world;
    WgrVec4 center;
    uint32_t model;
    uint32_t flags;
    /* Inflated frustum-cull radius (float bits) for terrain-conform instances, whose displaced
     * geometry escapes the flat model sphere; 0 = rigid (cull uses model bounding sphere). */
    uint32_t cull_radius;
    uint32_t _pad;
    /* Terrain-conform plane (mirrors WgrDraw3D::conform*). conform2.z = mode: 0 rigid,
     * 1 = ForestPlain bilinear plane, 2 = per-vertex ClipLand SurfaceY (conform0.x = bcSurfaceY). */
    WgrVec4 conform0;
    WgrVec4 conform1;
    WgrVec4 conform2;
};

static_assert(sizeof(WgrModelSection) == 24, "WgrModelSection must match Rust");
static_assert(sizeof(WgrModelMaterial) == 80, "WgrModelMaterial must match Rust");
static_assert(sizeof(WgrModelLod) == 16, "WgrModelLod must match Rust");
static_assert(sizeof(WgrInstance) == 144, "WgrInstance must match Rust");

/* Live tonemap/look parameters, pushed via wgr_set_tonemap (from the ImGui Tonemap
 * tab). The Hable curve is fixed in the shader; these are exposure + the colour-grade
 * block. Layout matches the Rust WgrTonemap #[repr(C)] and the tonemap.wgsl uniform. */
struct WgrTonemap
{
    float exposure;    /* linear pre-curve multiplier */
    float mode;        /* 0 = passthrough (clamp), 1 = Hable */
    float encode;      /* 0 = write as-is, 1 = linear->sRGB encode */
    float temperature; /* white balance warm(+)/cool(-) */
    float tint;        /* white balance magenta(+)/green(-) */
    float contrast;    /* post-curve contrast (1 = neutral) */
    float saturation;  /* post-curve saturation (1 = neutral) */
    float lift;        /* shadow lift (0 = neutral) */
    float gain;        /* post-curve overall multiply (1 = neutral) */
    float bloom_intensity; /* linear weight of the bloom added to the scene (0 = off) */
    float bloom_threshold; /* bloom soft-knee centre (scene-referred luminance) */
    float bloom_knee;      /* bloom soft-knee half-width */
};

/* Eye-adaptation / auto-exposure parameters, pushed via wgr_set_exposure. Layout
 * matches the Rust WgrExposure #[repr(C)] and exposure.wgsl's ExpParams (8 f32). */
struct WgrExposure
{
    float enabled;   /* 0 = off (scale eases to 1.0), 1 = auto-exposure on */
    float key;       /* target middle-grey luminance (higher = brighter) */
    float min_scale; /* clamp on the exposure multiplier */
    float max_scale;
    float rate;       /* per-frame ease toward the target (0..1) */
    float sky_weight; /* metering weight of the top of frame (sky) vs bottom (ground) */
    float _pad1;
    float _pad2;
};

/* Procedural sky parameters, pushed via wgr_set_sky. The celestial fields
 * (sun/moon direction, night factor) are refreshed every frame from LightSun; the
 * atmosphere + look fields are authored and tuned in the ImGui Sky tab. The renderer
 * combines these with the per-frame inverse view-projection into the sky pass.
 * Layout matches the Rust WgrSky #[repr(C)] (7 vec4). See docs/procedural-sky-plan.md. */
struct WgrSky
{
    WgrVec4 sun_dir;       /* xyz = unit dir TO the sun (up by day); w = sun radiance scale */
    WgrVec4 moon_dir;      /* xyz = unit dir TO the moon; w = moon phase (0.5 = full) */
    WgrVec4 rayleigh;      /* xyz = Rayleigh scattering coeff (1/m); w = Rayleigh scale height (m) */
    WgrVec4 mie;           /* x = Mie scattering coeff, y = Mie g, z = Mie scale height (m), w = turbidity */
    WgrVec4 ground_albedo; /* xyz = ground albedo; w = night factor (0 day .. 1 night) */
    WgrVec4 params;        /* x = sun angular radius (rad), y = exposure, z = planet radius (m), w = atmosphere (m) */
    WgrVec4 control;       /* x = enabled, y = view samples, z = light samples, w = pad */
    WgrVec4 fog_color;     /* xyz = scene fog colour; w = horizon-haze strength (0 = off) */
    /* Authored night-sky floor (plan Stage 6): a deep-blue radiance blended in by sun
     * altitude so twilight/night settle into blue instead of near-black. */
    WgrVec4 night_zenith;  /* xyz = night radiance at the zenith, w = camera altitude ASL (m; aerial/sky raymarch origin) */
    WgrVec4 night_horizon; /* xyz = night radiance at the horizon */
    WgrVec4 night_params;  /* x = full-day sun_dir.y, y = full-night sun_dir.y, z = intensity, w = far-fade range (m; aerial dissolves the terrain edge into sky by this dist, 0 = off) */
};

/* Frame-global scalars carried in the camera UBO so the 3D shader can read them
 * without a 5th bind group (wgpu's default maxBindGroups is 4). Distinct concerns
 * (distance fog, shadow darkening) that happen to share this ride for that reason.
 * shadow_strength = GetShadowFactor()/256, read by WGR_BLEND_SHADOW draws. */
struct WgrFrameParams
{
    float fog_start;
    float fog_inv_range;
    float fog_enabled; // 0 = off, 1 = on
    float shadow_strength;
};

/* Per-camera cascaded-shadow sampling block, read by the lit 3D shaders. All
 * zeros (ctl.x = cascade count = 0 -> disabled) when shadow maps are off or for
 * UI/screen cameras; the depth pass itself is driven by WgrShadowPass, not this. */
struct WgrCameraShadow
{
    WgrMat4 cascade_vp[4]; // camera-relative light view-projections (0..1 NDC z)
    WgrVec4 splits;        // frustum tiers: far eye-depth per tier
    WgrVec4 omni_radius;   // omni tiers: camera-distance radius (0 = frustum tier)
    WgrVec4 ctl;           // {count, omni_count, fade_range, bias_const}
    WgrVec4 ctlb;          // {texel_size (1/res), darkness, normal_offset_scale, pcf}
    WgrVec4 cam_fwd;       // xyz = camera forward (eye-depth cascade select)
    WgrVec4 sun_dir;       // xyz = sun travel direction (normal-offset bias)
};

/* A view + projection pair, plus the frame-global params (see WgrFrameParams).
 * fog_color = rgb (+pad). */
struct WgrCamera
{
    WgrMat4 proj;
    WgrMat4 view;
    WgrVec4 fog_color;
    WgrFrameParams params;
    WgrCameraShadow shadow;
    /* World-space camera position. `view` has no translation (geometry is
     * camera-relative); terrain uses this to sample the world-space heightmap. */
    WgrVec4 cam_pos;
    /* Sun light for GPU-lit paths (terrain): rgb, pre-multiplied by the eye
     * accommodation — DoLightingColorized's DiffusePrecalc/AmbientPrecalc for a
     * white material. */
    WgrVec4 sun_diffuse;
    WgrVec4 sun_ambient;
    /* xyz = normalized MainLight()->Direction(): the sun's light TRAVEL
     * direction (downward by day, upward while the sun is below the horizon —
     * which is what keeps night terrain ambient-only). Same convention as
     * GL33's sunDir constant; shaders dot the normal with its negation. Valid
     * every frame, unlike the shadow block's sun_dir. */
    WgrVec4 sun_dir_world;
};

/* One shadow caster for the cascade depth passes: a section run
 * [index_begin, index_begin+index_count) of `mesh`, transformed by the
 * camera-relative `world` (or, when `palette_slot` is valid, GPU-skinned by that
 * palette block exactly like a WgrDraw3D). `alpha_ref` > 0 alpha-tests the
 * caster texture so cutout foliage casts a leaf silhouette instead of a blob. */
struct WgrShadowCaster
{
    WgrMesh mesh;
    uint32_t index_begin;
    uint32_t index_count;
    WgrMat4 world;
    WgrTexture texture_id; // sampled only when alpha_ref > 0; 0 = built-in white
    uint32_t palette_slot; // WGR_NO_PALETTE = rigid
    float alpha_ref;       // 0 = solid caster; > 0 = discard below (cutout)
    uint32_t sampler;
    uint32_t cascade_mask; // bit c set = render into cascade c
    // Terrain-conform plane for this caster (mirrors WgrDraw3D::conform*). Mode 2
    // (conform2.z) conforms ClipLand vegetation to SurfaceY per vertex in the depth
    // shader, so the shared shadow mesh is uploaded ONCE undeformed. 0 = rigid.
    WgrVec4 conform0; // x = bcSurfaceY
    WgrVec4 conform2; // z = mode
};

/* Cascade depth-pass parameters for one frame. The renderer draws
 * WgrFrame.shadow_casters into a `count`-layer depth array from these
 * camera-relative light view-projections before replaying the frame's command
 * stream. count = 0 disables the pass (and keeps last frame's map unused). */
struct WgrShadowPass
{
    uint32_t count; // cascade count (1..4); 0 = no shadow pass this frame
    uint32_t omni_count;
    uint32_t resolution; // depth-map side length per cascade
    uint32_t _pad;
    WgrMat4 light_vp[4]; // camera-relative light view-projections (0..1 NDC z)
    // Camera world position: casters are camera-relative, so the depth shader adds
    // this back to reconstruct absolute world xz for surface_y (terrain conform).
    WgrVec4 cam_pos;
};

/* One entry in the frame's submission-ordered command stream. */
struct WgrCmd
{
    WgrCmdKind kind;
    uint32_t arg;
};

// --- Terrain (GPU heightmap) -------------------------------------------------

/* Per-map terrain parameters, uploaded once with the heightmap. The heightmap is
 * an hm_width x hm_height R32Float texture of world heights sampled in the vertex
 * shader; `terrain_grid` is the world spacing between adjacent heightmap texels,
 * `land_grid` the coarser texture-cell spacing, `world_origin` the world-space xz
 * of texel (0,0). `data_scale` is currently unused (heights arrive in metres). */
struct WgrTerrainParams
{
    WgrVec2 world_origin;
    float land_grid;
    float terrain_grid;
    uint32_t hm_width;
    uint32_t hm_height;
    uint32_t land_range; // land-cell count per axis
    float data_scale;
    /* Coast wet band (Stage 2c), pushed per frame via wgr_terrain_set_params. sea_level + time
     * (+ swash) move the damp intertidal line in lockstep with the water's edge; wet_height =
     * metres above the (swash-moved) sea level the band reaches, wet_darken = albedo multiplier
     * in the band (1 = off). Slope-gated in the shader; uses the SAME swash formula/params as
     * the water shader so the two register. */
    float sea_level;
    float time;
    float swash_speed;
    float swash_amp;
    float wet_height;
    float wet_darken;
    float _pad0;
    float _pad1;
};

/* One terrain node instance: the shared grid mesh placed at world-xz `origin`,
 * covering `size` x `size` world units, at level `lod`. `morph_start`/`morph_end`
 * are the camera-distance band over which the grid morphs toward its coarser parent. */
struct WgrTerrainNode
{
    WgrVec2 origin;
    float size;
    uint32_t lod;
    float morph_start;
    float morph_end;
};

/* A run [first_node, first_node+node_count) of WgrFrame.terrain_nodes drawn with
 * the shared grid mesh, transformed by camera `camera` (indexes WgrFrame.cameras). */
struct WgrTerrainBatch
{
    uint32_t first_node;
    uint32_t node_count;
    uint32_t camera;
    uint32_t _pad;
};

// --- Water (GPU CDLOD surface) -----------------------------------------------

/* Per-map + per-frame water parameters (a small UBO). `world_origin`/`terrain_grid`/
 * `hm_width`/`hm_height` describe the terrain heightmap the sibling look plan samples
 * for shoreline depth (unused by the flat-plane geometry pass); `sea_level` is the
 * animated global sea height (Landscape::GetSeaLevel) and `time` the wave-animation
 * clock (seconds). The look block (wave_amp..alpha) is the live-tunable water look,
 * edited by the ImGui Water tab; `fade_start`/`fade_end` flatten wave detail with
 * distance (metres) to kill far-field moiré. All refreshed every frame. */
struct WgrWaterParams
{
    WgrVec2 world_origin;
    float terrain_grid;
    float sea_level;
    uint32_t hm_width;
    uint32_t hm_height;
    float time;
    float wave_amp;
    float wave_choppy;
    float wave_speed;
    float wave_scale;
    float fade_start;
    float fade_end;
    float warp_amp;
    float spec_power;
    float spec_intensity;
    float alpha;
    float shadow_dim;
    /* Depth-based colour + soft shoreline (Stage 2). color_ext = 1/m extinction: how fast the
     * body tint saturates shallow -> deep with the water column depth. coast_fade = metres of
     * column depth over which the shoreline ramps transparent -> opaque. */
    float color_ext;
    float coast_fade;
    /* rgb = shallow / deep body colour (gamma-space; decoded to linear on HDR). w unused. */
    WgrVec4 shallow_color;
    WgrVec4 deep_color;
    /* Coast foam + swash (Stage 2c). foam_width = m of column depth over which shoreline foam
     * fades out; foam_intensity scales it. swash_amp = m the near-shore waterline oscillates
     * in/out; swash_speed = cycles/s. Cosmetic (buoyancy stays on the flat plane). */
    float foam_width;
    float foam_intensity;
    float swash_amp;
    float swash_speed;
};

/* One water node instance: byte-identical to WgrTerrainNode (the shared grid mesh
 * placed at world-xz `origin`, `size` x `size`, level `lod`, morphing over the
 * `morph_start`/`morph_end` camera-distance band). A distinct type so the two can
 * evolve independently (the look plan adds per-node flags). */
struct WgrWaterNode
{
    WgrVec2 origin;
    float size;
    uint32_t lod;
    float morph_start;
    float morph_end;
};

/* A run [first_node, first_node+node_count) of WgrFrame.water_nodes drawn with the
 * shared grid mesh, transformed by camera `camera` (indexes WgrFrame.cameras). */
struct WgrWaterBatch
{
    uint32_t first_node;
    uint32_t node_count;
    uint32_t camera;
    uint32_t _pad;
};

/* Overlay (dev panel / ImGui) vertex: framebuffer pixels, top-left origin.
 * `color` is RGBA with R in the low byte (ImGui packing, NOT WgrRgba8). */
struct WgrOverlayVertex
{
    WgrVec2 pos;
    WgrVec2 uv;
    uint32_t color;
};

/* One scissored overlay draw: `index_count` indices from
 * WgrFrame.overlay_indices starting at `first_index`, offset by `base_vertex`
 * into WgrFrame.overlay_verts, clipped to `clip` = {x0, y0, x1, y1} pixels. */
struct WgrOverlayDraw
{
    WgrVec4 clip;
    WgrTexture texture_id; // 0 = built-in white
    uint32_t first_index;
    uint32_t index_count;
    uint32_t base_vertex;
    uint32_t _pad;
};

// --- Frame -------------------------------------------------------------------

/* Everything needed to render + present one frame. The renderer clears to
 * `clear` (+depth), then replays `cmds` in submission order: each 2D batch and
 * 3D draw renders interleaved exactly as recorded, so 3D UI elements land
 * between their 2D background and foreground. WGR_CMD_CLEAR_DEPTH starts a new
 * segment with a freshly cleared depth buffer (colour preserved). 3D draws are
 * depth-tested and transformed by `cameras[draw.camera]`. `fog_color` is what
 * each vertex's `fog` blends toward. Any slice may be empty. */
struct WgrFrame
{
    WgrVec4 clear;
    WgrVec3 fog_color;

    WgrSlice<WgrCamera> cameras;
    WgrSlice<WgrDraw3D> draws3d;
    WgrSlice<WgrVertex2D> verts;
    WgrSlice<WgrDraw2DBatch> batches;
    WgrSlice<WgrCmd> cmds;
    /* Bone-matrix pool for skinned draws: one 128-matrix block per palette slot,
     * world already pre-multiplied in (palette[i] = world * boneMatrix[i]). Length is a
     * multiple of 128. Empty if no skinned draws. */
    WgrSlice<WgrMat4> palette;

    /* Cascaded-shadow depth pass: rendered before the command stream when
     * shadow.count > 0 and shadow_casters is non-empty. */
    WgrShadowPass shadow;
    WgrSlice<WgrShadowCaster> shadow_casters;

    /* Overlay (dev panel): alpha-blended over the finished frame, no depth. */
    WgrSlice<WgrOverlayVertex> overlay_verts;
    WgrSlice<uint16_t> overlay_indices;
    WgrSlice<WgrOverlayDraw> overlay_draws;

    /* GPU terrain nodes, drawn on WGR_CMD_DRAW_TERRAIN. The heightmap + ground
     * textures are uploaded separately via wgr_terrain_*. */
    WgrSlice<WgrTerrainNode> terrain_nodes;
    WgrSlice<WgrTerrainBatch> terrain_batches;

    /* Frame-global point/spot lights (<= 256), uploaded once into the group-0
     * storage buffer shared by 3D draws + terrain. The per-camera light count
     * rides in WgrCamera::cam_pos.w. */
    WgrSlice<WgrLight> lights;

    /* GPU water nodes, drawn on WGR_CMD_DRAW_WATER. Placement params (incl. the
     * per-frame sea level) are uploaded separately via wgr_water_set_params. */
    WgrSlice<WgrWaterNode> water_nodes;
    WgrSlice<WgrWaterBatch> water_batches;
};

// --- Layout guards (mirror rust/src/ffi.rs) ----------------------------------

static_assert(sizeof(WgrVec2) == 8, "WgrVec2 must be 2 floats");
static_assert(sizeof(WgrVec3) == 12, "WgrVec3 must be 3 floats");
static_assert(sizeof(WgrVec4) == 16, "WgrVec4 must be 4 floats");
static_assert(sizeof(WgrMat4) == 64, "WgrMat4 must be 16 floats");
static_assert(sizeof(WgrSlice<WgrCamera>) == 16 && alignof(WgrSlice<WgrCamera>) == 8,
              "WgrSlice must be a { pointer, u32 } with 8-byte alignment");
static_assert(sizeof(WgrBlend) == 4, "WgrBlend must be 4 bytes to match the Rust #[repr(u32)] enum");
static_assert(sizeof(WgrVertex2D) == 32, "WgrVertex2D layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrMeshVertex) == 36, "WgrMeshVertex must match the engine SVertex layout");
static_assert(sizeof(WgrDraw2DBatch) == 32, "WgrDraw2DBatch layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrDraw3D) == 264, "WgrDraw3D layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrLight) == 64, "WgrLight layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrTonemap) == 48, "WgrTonemap layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrExposure) == 32, "WgrExposure layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrSky) == 176, "WgrSky layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrFrameParams) == 16, "WgrFrameParams layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrCameraShadow) == 352, "WgrCameraShadow layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrCamera) == 576, "WgrCamera layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrShadowCaster) == 136, "WgrShadowCaster layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrShadowPass) == 288, "WgrShadowPass layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrCmd) == 8, "WgrCmd layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrOverlayVertex) == 20, "WgrOverlayVertex layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrOverlayDraw) == 40, "WgrOverlayDraw layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrTerrainParams) == 64, "WgrTerrainParams layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrTerrainNode) == 24, "WgrTerrainNode layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrTerrainBatch) == 16, "WgrTerrainBatch layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrWaterParams) == 128, "WgrWaterParams layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrWaterNode) == 24, "WgrWaterNode layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrWaterBatch) == 16, "WgrWaterBatch layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrFrame) == 560, "WgrFrame layout must match the Rust #[repr(C)] struct");

// --- Functions ---------------------------------------------------------------

extern "C"
{
    WGR_API const char* wgr_version(void);

    /* Returns NULL on failure (reason reported via `log` if supplied). `log` may be NULL. */
    WGR_API WgrRenderer* wgr_create(const WgrSurfaceDesc* desc, const WgrLogCallbacks* log);

    WGR_API void wgr_destroy(WgrRenderer* renderer);
    WGR_API void wgr_resize(WgrRenderer* renderer, uint32_t width, uint32_t height);

    /* Upload a texture in `format` (WgrTextureFormat); returns a non-zero id, or
     * 0 on failure. `data` holds `mip_count` tightly packed mip levels, level i
     * sized for (max(1, width>>i), max(1, height>>i)): RGBA8 = w*h*4 per level;
     * BC* = the block-payload size (ceil(w/4)*ceil(h/4) * 8 for BC1, * 16 for
     * BC2/BC3). `byte_length` is the total. Pass WGR_TEXTURE_GEN_MIPS in `flags`
     * (RGBA8, mip_count 1 only) to generate the rest of the chain with a box
     * filter. */
    WGR_API WgrTexture wgr_texture_create(WgrRenderer* renderer, uint32_t width, uint32_t height, int32_t format,
                                          uint32_t mip_count, uint32_t flags, const uint8_t* data,
                                          uint32_t byte_length);

    constexpr uint32_t WGR_TEXTURE_GEN_MIPS = 1;

    /* Replace the pixels of an existing RGBA8 texture. */
    WGR_API void wgr_texture_update(WgrRenderer* renderer, WgrTexture id, const uint8_t* rgba, uint32_t byte_length);

    WGR_API void wgr_texture_destroy(WgrRenderer* renderer, WgrTexture id);

    /* Create a static mesh from interleaved vertices + 16-bit triangle-list
     * indices; returns a non-zero handle, or 0 on failure. */
    WGR_API WgrMesh wgr_mesh_create(WgrRenderer* renderer, WgrSlice<WgrMeshVertex> verts, WgrSlice<uint16_t> indices);

    /* Re-upload vertex data for an existing mesh (dynamic / animated shapes).
     * The topology (indices) is unchanged; the vertex count must not exceed the
     * mesh's original vertex count. */
    WGR_API void wgr_mesh_update(WgrRenderer* renderer, WgrMesh id, WgrSlice<WgrMeshVertex> verts);

    /* Attach per-vertex skinning data to a mesh: 4 bone indices + 4 quantised
     * weights per vertex (each buffer `4 * vert_count` bytes). Weights are
     * Unorm8x4 (0..255 -> 0..1) and should sum to ~1 per vertex. */
    WGR_API void wgr_mesh_set_skin(WgrRenderer* renderer, WgrMesh id, WgrSlice<uint8_t> bones,
                                   WgrSlice<uint8_t> weights);

    WGR_API void wgr_mesh_destroy(WgrRenderer* renderer, WgrMesh id);

    /* --- GPU-driven retained scene (docs/gpu-culling-and-depth-plan.md Stage 3b) --- */

    /* Sentinel returned by wgr_model_register on failure. */
    constexpr uint32_t WGR_INVALID_MODEL = 0xFFFFFFFFu;

    /* Register one opaque-rigid model for GPU-driven rendering. `lods`, `sections`, and
     * `materials` describe a single LODShapeWithShadow: `sections` and `materials` are
     * parallel (one material per section) and each lods[i].section_base indexes
     * `sections` relative to this model. Section mesh handles are resolved to the
     * shared geometry pool. Returns the model id (for wgr_instance_add) or
     * WGR_INVALID_MODEL on error. Call once per shape. */
    WGR_API uint32_t wgr_model_register(WgrRenderer* renderer, float bounding_sphere, WgrSlice<WgrModelLod> lods,
                                        WgrSlice<WgrModelSection> sections, WgrSlice<WgrModelMaterial> materials);

    /* Add a static retained instance; returns its stable slot (recycled from removed
     * slots). Update it in place with wgr_instance_update (a move, or a destruction-
     * phase change), remove it with wgr_instance_remove. */
    WGR_API uint32_t wgr_instance_add(WgrRenderer* renderer, const WgrInstance* inst);
    WGR_API void wgr_instance_update(WgrRenderer* renderer, uint32_t slot, const WgrInstance* inst);
    WGR_API void wgr_instance_remove(WgrRenderer* renderer, uint32_t slot);

    /* Replace the whole dynamic instance set for this frame (the churny set the CPU
     * already walks for simulation: vehicles, units, ...). Re-copied wholesale each
     * frame. */
    WGR_API void wgr_set_dynamic(WgrRenderer* renderer, WgrSlice<WgrInstance> instances);

    /* Push this frame's engine-derived cull + LOD inputs (the real Scene::LevelFromDistance2
     * values): `objects_z` = ENGINE_CONFIG.objectsZ draw distance, `lod_scale` = Camera::Left()
     * (projection tan(halfFovX)), `lod_inv_width` = Scene::GetLodInvWidth()
     * (~ lodCoef*2/screenWidth), `pixel_limit` = the legacy 0.125 sub-pixel threshold. No-op
     * unless GPU-driven rendering is enabled. Call once per frame for the main scene camera. */
    WGR_API void wgr_set_cull_params(WgrRenderer* renderer, float objects_z, float lod_scale,
                                     float lod_inv_width, float pixel_limit);

    /* Per-frame gate for the retained GPU-driven world set. When `suppress` is true the
     * renderer skips the GPU-driven object draws (colour + prepass) for the frame, so the
     * editor / loading / shutdown frames letterbox to black instead of leaking clutter behind
     * the 2D UI. Resources stay resident; only the draw submission is skipped. No-op unless
     * GPU-driven rendering is enabled. Call every frame with the current state. */
    WGR_API void wgr_set_suppress_world_objects(WgrRenderer* renderer, bool suppress);

    /* Debug/feature toggles for the GPU-driven cull (ImGui Culling tab): `draw_spheres` renders
     * the per-instance frustum-cull sphere wireframes on top of the scene; `no_frustum` skips the
     * GPU frustum test entirely (a "is the cull dropping it?" discriminator); `occlusion` enables
     * GPU Hi-Z occlusion culling (docs/gpu-culling-and-depth-plan.md §5 — the color pass draws
     * only the retained objects not hidden by the depth-prepass occluders). No-op unless GPU-driven
     * rendering is enabled. */
    WGR_API void wgr_set_cull_debug(WgrRenderer* renderer, bool draw_spheres, bool no_frustum, bool occlusion);

    /* Upload (or replace) the terrain heightmap: `heights` is
     * params->hm_width * params->hm_height row-major world-height floats (row 0 =
     * texel z=0). Creates the R32Float heightmap texture + params UBO, once per
     * map load. */
    WGR_API void wgr_terrain_set_heightmap(WgrRenderer* renderer, const float* heights,
                                           const WgrTerrainParams* params);

    /* Refresh the terrain params UBO without re-uploading the heightmap. Cheap; called every
     * frame to animate the coast wet band (sea_level / time / swash / wet_*). */
    WGR_API void wgr_terrain_set_params(WgrRenderer* renderer, const WgrTerrainParams* params);

    /* Set the terrain ground layers: `handles[i]` is the wgr_texture_create
     * handle for Landscape texture index i (0 = the built-in white fallback).
     * Layers keep their native size/format/mips; the fragment shader samples
     * them through a bindless binding_array, indexed per land cell by
     * wgr_terrain_set_index_map. At most WGR_TERRAIN_MAX_GROUND_LAYERS are
     * used; the index-map upload must clamp cell indices to the same bound. */
    WGR_API void wgr_terrain_set_ground_layers(WgrRenderer* renderer, const uint64_t* handles, uint32_t count);

    constexpr uint32_t WGR_TERRAIN_MAX_GROUND_LAYERS = 512;

    /* Upload the per-land-cell texture index map: a `width` x `height` (= land
     * range per axis) R16Uint texture where each texel's bits 0-14 are the
     * ground-layer index for that land cell (row 0 = cell z=0; index 0 = sea).
     * Bit 15 marks a clamped transition tile: its texture maps exactly once onto
     * the cell with edges extended (GL33's ClampU|ClampV) instead of tiling.
     * `indices` is width*height uint16s. */
    WGR_API void wgr_terrain_set_index_map(WgrRenderer* renderer, uint32_t width, uint32_t height,
                                           const uint16_t* indices);

    /* Upload the per-grid-point ground UV jitter map: a `width` x `height`
     * (= land range per axis) Rg8Snorm texture holding each land grid point's
     * random texture UV offset (Landscape::_random, at most +-0.7). The fragment
     * shader interpolates it bilinearly across each cell and adds it to the
     * ground tiling UV, replicating GL33's per-vertex jitter. `offsets` is
     * width*height (u, v) int8 pairs (snorm: value / 127). */
    WGR_API void wgr_terrain_set_jitter_map(WgrRenderer* renderer, uint32_t width, uint32_t height,
                                            const int8_t* offsets);

    /* Set the high-frequency detail noise texture tiled over the terrain
     * (OFP's `CfgDetailTextures.detail`) to a wgr_texture_create handle; its
     * alpha channel modulates the blended ground colour (rgb *= 2*detail.a).
     * Handle 0 is ignored (the neutral built-in stand-in stays). */
    WGR_API void wgr_terrain_set_detail_layer(WgrRenderer* renderer, WgrTexture handle);

    /* Live-tune the long-distance terrain sun-shadow sweep (heightfield self-
     * shadow, a complement to the cascade maps). `strength` scales the occlusion
     * (0 = off, 1 = physical, >1 = exaggerated for debugging); `scale` is the mask
     * supersample factor over the heightmap grid (>=1, higher = sharper edges,
     * more VRAM); `max_steps` caps the march range (steps * terrain_grid metres);
     * `penumbra_deg` is the soft-edge half-width in degrees. Changing `scale`
     * reallocates the mask; any change re-runs the amortized sweep next frame. */
    WGR_API void wgr_terrain_set_sun_shadow(WgrRenderer* renderer, float strength, uint32_t scale,
                                            uint32_t max_steps, float penumbra_deg);

    /* Live-tune the terrain sky-visibility (sky-view factor) ambient occlusion — the AO complement to
     * the sun-shadow sweep, darkening the ambient in valleys/gorges/cove-water/cliff-bases (terrain,
     * objects and water). `strength` scales the effect (0 = off), `contrast` deepens the occlusion for
     * the near-1 factor smooth terrain yields (1 = physical/linear, higher = punchier), `floor` keeps
     * a minimum ambient in fully-occluded columns; these three are cheap per-frame uniform values.
     * `radius_m`, `k_azimuths` and `downsample` (output-grid coarseness, >=1; 1 = per-heightmap-texel,
     * sharpest) shape the CPU horizon scan and re-run it (from the retained heightfield) ONLY when they
     * change. `debug` makes terrain output the (contrast-shaped) sky-view factor as greyscale.
     * See docs/sky-visibility-ambient-plan.md. */
    WGR_API void wgr_terrain_set_sky_visibility(WgrRenderer* renderer, float strength, float contrast,
                                                float floor, float radius_m, uint32_t k_azimuths,
                                                uint32_t downsample, bool debug);

    /* Set/refresh the water placement params (see WgrWaterParams). Cheap; called on
     * map load and each frame to update the animated `sea_level`. */
    WGR_API void wgr_water_set_params(WgrRenderer* renderer, const WgrWaterParams* params);

    /* Render + present one frame. Returns 0 on success (incl. transient skipped
     * frames), negative on error. */
    WGR_API int32_t wgr_render_frame(WgrRenderer* renderer, const WgrFrame* frame);

    /* Set the live tonemap/look parameters (exposure, curve, Hable params, output
     * gain). Takes effect on the next frame's resolve; no-op on the LDR-direct path. */
    WGR_API void wgr_set_tonemap(WgrRenderer* renderer, const WgrTonemap* params);
    WGR_API void wgr_set_exposure(WgrRenderer* renderer, const WgrExposure* params);
    WGR_API float wgr_get_exposure_scale(WgrRenderer* renderer);

    /* Set the procedural sky parameters (celestial + authored look). Pushed per
     * frame for the celestial fields and on edit from the ImGui Sky tab. Takes
     * effect next frame; the sky pass is skipped when control.x (enabled) is 0. */
    WGR_API void wgr_set_sky(WgrRenderer* renderer, const WgrSky* params);

    /* Read one cascade layer of the shadow depth map back as row-major floats
     * (row 0 = the top texture row). Returns the map resolution (side length),
     * or 0 when no map has been rendered / `layer` is out of range / `out_len`
     * is smaller than resolution². Debug/test hook (DumpShadowMap). */
    WGR_API uint32_t wgr_shadow_map_read(WgrRenderer* renderer, uint32_t layer, float* out, uint32_t out_len);

    /* Render `vert_count` triangle-list vertices (xyz, 3 floats each) through
     * the shadow depth pipeline with the given column-major light
     * view-projection into a scratch res*res depth map, and read it back into
     * `out` (res*res floats, row 0 = top). Returns 1 on success. Debug/test
     * hook (ShadowDepthProbe: CPU-reference parity for the depth path). */
    WGR_API int32_t wgr_shadow_depth_probe(WgrRenderer* renderer, const float* light_vp16, const float* tri_xyz,
                                           uint32_t vert_count, uint32_t res, float* out);

} // extern "C"

#endif // WGPU_RENDERER_HPP
