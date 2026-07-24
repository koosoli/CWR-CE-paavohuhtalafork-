The document below assumes the agents have seen \*\*none\*\* of the earlier plans. It starts from the water implementation currently present on your GitHub branch and tells them how to turn that implementation into the strongest practical water system for CWR-CE.



\# CWR-CE Water System Master Plan



\## Implementation specification based on the current `new-renderer-infrastructure` branch



\*\*Repository:\*\* `koosoli/CWR-CE-paavohuhtalafork-`

\*\*Target branch:\*\* `new-renderer-infrastructure`

\*\*Primary objective:\*\* Upgrade the water system already present in the branch into a coherent, scalable, interactive ocean and underwater-rendering system suitable for large Arma-style environments.



\---



\# 1. Purpose of this document



This is not a proposal to replace the existing water renderer.



The branch already contains:



\* A camera-relative CDLOD ocean surface

\* Four 256×256 FFT wave cascades

\* Gerstner fallback waves

\* Horizontal choppy displacement

\* FFT-derived slopes, curvature, crest and Jacobian information

\* A persistent foam field

\* A camera-relative ripple and interaction simulation

\* Bullet, object, player, explosion, footstep and continuous interaction event types

\* Environment-sky reflections

\* Screen-space reflections

\* Reflected-camera planar reflections

\* Depth-aware water colouring

\* Screen-space seabed refraction

\* Shoreline foam and swash

\* An underwater post compositor

\* Volumetric clouds

\* An atmospheric froxel volume that already contains infrastructure for volumetric lighting and terrain-shadowed shafts



The task is to \*\*improve, connect and complete these existing systems\*\*, not start over.



The implementation must preserve:



\* Gerstner fallback

\* Existing water controls

\* Existing C++→Rust FFI paths

\* Existing water interaction producers

\* Existing reflected-camera path

\* Existing sky and cloud renderer

\* Existing terrain and object shadow systems

\* Existing HDR, bloom, exposure and tonemap ordering



\---



\# 2. Verified state of the current implementation



\## 2.1 CPU-side ocean setup



`WaterWgpu.cpp` currently:



\* Builds an oversized CDLOD water quadtree

\* Extends the ocean beyond the terrain map

\* Uses a 32×32 grid per selected node

\* Pushes water appearance parameters every frame

\* Supplies four fixed FFT cascade lengths:



&#x20; \* 48 metres

&#x20; \* 144 metres

&#x20; \* 432 metres

&#x20; \* 1,296 metres

\* Uses a fixed deterministic wind direction and sea-state configuration because weather is not yet connected

\* Uses one global ocean water kind

\* Disables directed flow until separate water-body batching exists

\* Maintains a camera-relative 256-metre interaction domain

\* Snaps that domain in four-metre increments

\* Uses a 256×256 interaction grid

\* Clamps interaction `dt` to 1/30 second

\* Receives externally submitted water interaction events

\* Uses a player-water-depth plus downward-camera-direction heuristic to activate the underwater compositor. (\[GitHub]\[1])



The header comments still describe the water geometry as a flat CDLOD plane, although the Rust vertex shader now displaces it with FFT or Gerstner waves. Those comments should be updated when the architecture is stabilised, but documentation cleanup is not itself a water-quality milestone. (\[GitHub]\[2])



\## 2.2 Surface shader



The current shader has two surface paths:



\* FFT waves when supported and enabled

\* Eight Gerstner waves as fallback



The FFT path sums all four displacement cascades and all four normal cascades. It samples them in stable absolute world-space coordinates, which is correct for preventing camera-relative phase movement. (\[GitHub]\[3])



The vertex shader:



1\. Starts from an undisplaced spectral/material coordinate

2\. Evaluates FFT or Gerstner displacement

3\. Adds the local interaction-field height

4\. Outputs the displaced world position

5\. Retains the original material coordinate for spectral lookups



That distinction must be preserved. The material coordinate and displaced world coordinate serve different purposes.



\## 2.3 Important distance limitation



The current `wave\_fade` applies one scalar fade to all wave displacement and all FFT normal slopes. Past `fade\_end`, the full ocean becomes flat. The code explicitly states that both short and long wave detail are removed to avoid distant moiré and visible repetition. (\[GitHub]\[3])



This avoids aliasing but removes:



\* Long swell at the horizon

\* Large-wave silhouettes at altitude

\* Broad low-frequency ocean motion

\* Natural variation in distant reflections



This is one of the first systems that should be corrected.



\## 2.4 Current FFT implementation



The FFT system currently uses:



\* Four layers

\* 256×256 resolution

\* Eight radix-2 stages

\* Three complex data packs

\* Two transform axes

\* 48 staged FFT dispatches per update

\* One spectrum dispatch

\* One composition dispatch

\* No GPU timestamp queries around the FFT pass. (\[GitHub]\[4])



The stage shader:



\* Performs bit reversal during stage zero

\* Calculates sine and cosine twiddle factors inside the shader

\* Uses separate ping-pong textures

\* Divides by (N^2) at the end of the second axis. (\[GitHub]\[5])



The spectrum shader currently generates the random Gaussian spectral amplitude and evolves it in the same per-frame pass. Its energy expression is a simplified Phillips-style wind-wave spectrum with directional spreading and cascade banding, rather than a complete Horvath/JONSWAP implementation with explicit peak enhancement, fetch and swell controls. (\[GitHub]\[6])



The composition pass already produces valuable physical data:



\* Vertical displacement

\* Horizontal displacement

\* Surface slopes

\* Curvature

\* Crest value

\* Horizontal-displacement Jacobian

\* Compression

\* Slope variance. (\[GitHub]\[7])



These outputs should be retained and expanded rather than discarded.



\## 2.5 Current interaction field



The existing interaction simulation:



\* Has a 256×256 field over 256×256 metres

\* Has approximately one-metre cells

\* Supports up to 48 active events

\* Defines bullet, object, player, explosion, footstep and continuous disturbance types

\* Supports point and capsule-shaped disturbances

\* Stores height, velocity and foam/aeration data

\* Propagates waves with a damped five-point Laplacian

\* Loops over every active event from every interaction texel

\* Limits the minimum event radius relative to cell size. (\[GitHub]\[8])



The system is therefore not missing. It is too coarse and too expensive to scale directly to the resolution needed for bullets and footsteps.



At approximately one metre per cell, a centimetre-scale projectile impact cannot be represented accurately.



\## 2.6 Current foam system



The persistent foam system already derives sources from:



\* Crest height

\* Jacobian compression

\* Curvature

\* Slope magnitude

\* Interaction-field aeration



It stores foam history, age and aeration. However, previous foam is primarily backtraced using wind motion, rather than the actual FFT surface velocity and interaction flow. (\[GitHub]\[9])



The final surface shader combines:



\* Procedural shoreline foam

\* Persistent foam

\* Immediate FFT breaker foam

\* Interaction foam

\* Procedural breakup noise



The individual layers use different movement rules, which can make the result look assembled rather than transported by one ocean.



\## 2.7 Current reflections



The water shader currently combines:



1\. Directional sky-environment reflection

2\. Screen-space reflection

3\. Reflected-camera planar reflection



SSR is evaluated first as the highest-detail on-screen source. Planar reflection fills SSR misses, while the directional environment remains the fallback. (\[GitHub]\[3])



The reflected-camera pass renders:



\* Atmospheric sky

\* Terrain

\* Objects

\* Volumetric clouds

\* A filtered planar mip chain. (\[GitHub]\[10])



The directional environment map also includes atmosphere and a cheaper cloud evaluation specifically intended for water reflection and ambient lighting. (\[GitHub]\[11])



Therefore, clouds can currently exist in both:



\* The planar reflected-camera texture

\* The directional sky-environment reflection



The planar shader validity is primarily based on projected UV and edge coverage rather than a geometry-only validity mask. This means the planar sky and cloud result can replace the directional environment even where there is no reflected terrain or object.



That overlap is a likely cause of unstable or camera-pitch-sensitive cloud reflection.



\## 2.8 Current refraction and transparency



The current water refraction:



\* Reconstructs whether opaque geometry lies behind the water

\* Offsets screen UV using the water normal

\* Samples the existing opaque-scene colour

\* Rejects obvious foreground-depth leaks

\* Uses depth-dependent colour and transmission



It does \*\*not\*\* trace a refracted ray from the displaced surface to a new seabed-depth intersection. (\[GitHub]\[3])



This is why the seabed can still appear like a flat image being distorted rather than geometry seen through a displaced refractive surface.



The current water material uses:



\* A scalar body-colour extinction control

\* Hard-coded Fresnel (F\_0=0.035)

\* An additional artistic reflection boost

\* Shallow and deep colour interpolation. (\[GitHub]\[3])



The system should be upgraded to RGB absorption and scattering with a separate physical Fresnel term and artistic controls.



\## 2.9 Current underwater compositor



The underwater path currently receives only:



\* Finished scene colour

\* Scene depth

\* A sampler

\* Time



It applies:



\* A small procedural screen-space distortion

\* An approximate depth conversion

\* Hard-coded RGB transmittance

\* A fixed blue-green haze

\* A procedural sine-based caustic multiplier

\* Exponential fog. (\[GitHub]\[12])



It currently receives no:



\* Sun direction

\* Sun radiance

\* CSM shadow maps

\* Terrain shadow mask

\* FFT surface normals

\* Surface transmission

\* Water optical parameters

\* Whitewater aeration

\* Underwater current

\* Volumetric-scattering volume



\## 2.10 Existing volumetric infrastructure relevant to god rays



The sky renderer already creates a 32×32×32 frustum-aligned atmospheric froxel volume.



That volume already:



\* Marches each view column front to back

\* Stores accumulated in-scattering and transmittance

\* Samples the long-range terrain sun-shadow mask

\* Has CSM bindings and a CSM occlusion function

\* Is explicitly designed to prevent sunlight passing through hills and to create volumetric shafts between occluders. (\[GitHub]\[11])



The underwater god-ray system should reuse this architecture and shadow plumbing, but it must use a separate underwater medium. Atmospheric density calculations must not be reused directly underwater.



\---



\# 3. Target architecture



The completed water system should consist of the following connected layers:



```text

Ocean geometry and spectrum

&#x20;   ├── FFT local wind sea

&#x20;   ├── FFT distant swell

&#x20;   ├── frequency-aware LOD

&#x20;   └── Gerstner fallback



Surface interaction

&#x20;   ├── near high-resolution field

&#x20;   ├── medium wake/explosion field

&#x20;   ├── moving pressure emitters

&#x20;   └── shoreline response



Water material

&#x20;   ├── shared surface state

&#x20;   ├── SSR

&#x20;   ├── planar reflected geometry

&#x20;   ├── directional sky/cloud reflection

&#x20;   ├── refracted-depth ray query

&#x20;   ├── RGB absorption/scattering

&#x20;   └── physically based Fresnel



Foam and whitewater

&#x20;   ├── persistent breaker foam

&#x20;   ├── wake and interaction foam

&#x20;   ├── shoreline foam

&#x20;   ├── spray

&#x20;   ├── surface foam particles

&#x20;   └── bubbles



Underwater

&#x20;   ├── correct displaced waterline

&#x20;   ├── RGB extinction

&#x20;   ├── underwater froxel volume

&#x20;   ├── terrain/object-shadowed god rays

&#x20;   ├── surface-focused caustics

&#x20;   ├── suspended particles

&#x20;   ├── bubbles and aeration

&#x20;   └── common lighting with surface water



Gameplay

&#x20;   ├── water-height query

&#x20;   ├── normal and velocity query

&#x20;   ├── camera submersion

&#x20;   ├── buoyancy

&#x20;   ├── wake placement

&#x20;   └── object-impact placement

```



\---



\# 4. Mandatory engineering rules



\## 4.1 Do not rewrite everything at once



Each pull request must change one principal concern.



Examples:



\* Distance filtering

\* Surface-state contract

\* Reflection ownership

\* Refraction

\* Interaction injection

\* Foam advection

\* Whitewater particle allocation

\* Underwater froxel lighting

\* Gameplay query

\* FFT optimisation



Do not combine several of these into one large “improve water” commit.



\## 4.2 Preserve fallback paths



During development retain:



\* Gerstner fallback

\* Existing FFT implementation

\* Existing simple refraction

\* Existing underwater compositor

\* Existing single interaction field

\* Existing foam field

\* Existing reflection composition



New paths must initially be runtime-selectable for A/B testing.



\## 4.3 Do not hide architectural problems with artistic retuning



Agents must not fix:



\* Sliding foam

\* Flat seabed appearance

\* Cloud reflection movement

\* Distant flattening

\* Weak impacts

\* Underwater lighting



by merely adding:



\* More procedural noise

\* Stronger colours

\* Extra normal maps

\* Larger Fresnel values

\* Additional arbitrary foam bands

\* Stronger blur

\* More screen distortion



The coordinate, simulation, ownership or lighting problem must be corrected first.



\## 4.4 Build deterministic diagnostics before visual changes



Every major water feature needs:



\* Frozen simulation time

\* Fixed random seed

\* Fixed camera

\* Fixed weather

\* Repeatable camera path

\* Before/after captures

\* GPU timings

\* Debug output



\## 4.5 Do not invent APIs



Before modifying a subsystem, an agent must:



1\. Read the complete target file.

2\. Search all callers.

3\. Trace the C++→FFI→Rust→WGSL path.

4\. Identify resource lifetime.

5\. Identify existing tests.

6\. Identify Gerstner and non-HDR fallbacks.

7\. Build the unmodified target.



If an exact engine operation cannot be found, the agent must stop and report the missing capability.



\---



\# 5. Phase WTR-000 — Instrumentation and deterministic testing



\## Objective



Create a reliable baseline before changing visual behaviour.



\## WTR-001 — Deterministic water debug mode



Add development controls for:



```text

fixed simulation time

fixed delta time

fixed FFT seed

freeze FFT

freeze interaction solver

freeze foam

freeze cloud movement

freeze weather

freeze whitewater particles

repeatable camera path

```



The same test frame should reproduce the same result between launches.



\## WTR-002 — GPU timestamp instrumentation



Add timestamps around:



\* Spectrum generation

\* Spectrum evolution

\* FFT horizontal stages

\* FFT vertical stages

\* FFT composition

\* Interaction injection

\* Interaction propagation

\* Foam update

\* Whitewater update

\* Planar sky

\* Planar terrain

\* Planar objects

\* Planar clouds

\* Planar mip generation

\* Water SSR

\* Water refraction

\* Water draw

\* Underwater froxel generation

\* Underwater composite

\* Caustic generation



Do not replace the FFT implementation before these timings exist.



\## WTR-003 — Water debug views



Add switchable debug views for:



1\. FFT displacement by cascade

2\. Horizontal displacement

3\. Vertical displacement

4\. FFT slope

5\. Jacobian

6\. Compression

7\. Curvature

8\. Crest energy

9\. Slope variance

10\. Material coordinate

11\. Displaced world coordinate

12\. Interaction height

13\. Interaction velocity

14\. Interaction foam/aeration

15\. Persistent foam source

16\. Persistent foam history

17\. Surface velocity

18\. Water-column depth

19\. SSR colour

20\. SSR confidence

21\. Planar colour

22\. Planar geometry validity

23\. Directional sky/cloud reflection

24\. Final reflection-source selection

25\. Refraction ray

26\. Refraction hit validity

27\. Refraction path length

28\. RGB transmittance

29\. Camera-to-surface signed distance

30\. Underwater extinction

31\. Underwater in-scattering

32\. God-ray shadow visibility

33\. Caustic intensity

34\. Whitewater particle state

35\. Whitewater pool occupancy

36\. Particle overflow



\## WTR-004 — Standard test scenes



Create reproducible scenes:



\### WTR-Test-01 — Seabed checkerboard



\* Shallow clear water

\* Sloped checkerboard seabed

\* Slow camera orbit

\* No foam

\* Fixed sun



Tests refraction and geometry coherence.



\### WTR-Test-02 — Cloud pitch



\* Calm water

\* Distinctive cloud formation

\* Fixed camera position

\* Camera pitch from −45° to +45°

\* Water normals optionally frozen



Tests cloud reflection ownership and pitch stability.



\### WTR-Test-03 — Ocean altitude



Camera heights:



\* 2 metres

\* 50 metres

\* 500 metres

\* 2 kilometres



Tests cascade filtering and horizon motion.



\### WTR-Test-04 — Projectile grid



Known impact positions and radii:



\* 5 cm

\* 10 cm

\* 20 cm

\* 50 cm

\* 1 m



Capture at 30, 60 and 120 FPS.



\### WTR-Test-05 — Boat wake



\* Constant straight movement

\* Acceleration

\* Deceleration

\* Turning

\* Several hull sizes



\### WTR-Test-06 — Explosion



\* Small

\* Medium

\* Large

\* Surface detonation

\* Shallow underwater detonation

\* Deep underwater detonation



\### WTR-Test-07 — Underwater light



\* Clear water

\* Turbid water

\* High sun

\* Low sun

\* Tree/building occluders

\* Terrain ridge

\* Cloud shadow



Tests god rays and volumetric shadowing.



\### WTR-Test-08 — Waterline



\* Stationary camera

\* Passing wave crests

\* Slow upward/downward camera movement

\* Player looking in all directions



\### WTR-Test-09 — Shoreline



\* Gentle beach

\* Steep shore

\* Irregular bay

\* Incoming waves at different angles



\### WTR-Test-10 — Weather transition



\* Calm to storm

\* Wind-direction change

\* Swell-direction change

\* Storm to calm



\## Phase acceptance



No architecture work begins until:



\* Test scenes are reproducible

\* Major GPU passes have timings

\* Debug views exist for the subsystem being changed

\* Before/after capture rules are documented



\---



\# 6. Phase WTR-010 — Shared water-surface state



\## Objective



Make all systems agree about the same water surface while preserving the distinction between spectral coordinates and displaced world positions.



\## 6.1 Coordinate contract



Use:



\* (q): material/spectral coordinate

\* (x): horizontally and vertically displaced world coordinate

\* (x\_{previous}): previous displaced world coordinate



The relationship is:



\[

x(q,t)=q+D(q,t)

]



Use (q) for:



\* FFT texture lookup

\* Phase-coherent spectral data

\* Spectral derivatives

\* Spectral foam source



Use (x) for:



\* Scene-depth intersection

\* Reflection origin

\* Refraction origin

\* World lighting

\* Shore distance

\* Object impact

\* Particle emission

\* Gameplay position



\## WTR-011 — Shared surface-state representation



Introduce an internal structure conceptually equivalent to:



```cpp

struct WaterSurfaceState

{

&#x20;   float2 materialPosition;

&#x20;   float3 meanPlanePosition;

&#x20;   float3 displacedPosition;

&#x20;   float3 previousDisplacedPosition;



&#x20;   float3 displacement;

&#x20;   float3 velocity;



&#x20;   float3 geometricNormal;

&#x20;   float3 shadingNormal;



&#x20;   float jacobian;

&#x20;   float compression;

&#x20;   float curvature;

&#x20;   float slopeVariance;

&#x20;   float crestEnergy;

&#x20;   float breakingEnergy;



&#x20;   float interactionHeight;

&#x20;   float interactionVelocity;

&#x20;   float aeration;

&#x20;   float foamDensity;

};

```



The exact C++, Rust and WGSL representations may differ, but field meanings must remain consistent.



\## WTR-012 — Previous displacement and surface velocity



Retain previous FFT displacement or derive velocity analytically from the spectral phase.



Output:



\[

v=\\frac{x(t)-x(t-\\Delta t)}{\\Delta t}

]



Surface velocity is needed for:



\* Foam advection

\* Whitewater

\* Boat-relative water velocity

\* Buoyancy drag

\* Motion vectors

\* Weather transitions



\## WTR-013 — World-to-material inversion



World-space events arrive at (x), while FFT data uses (q).



Use two or three fixed-point iterations:



```text

q0 = worldXZ

q1 = worldXZ - horizontalDisplacement(q0)

q2 = worldXZ - horizontalDisplacement(q1)

q3 = worldXZ - horizontalDisplacement(q2)

```



Use this only where needed:



\* Impact insertion

\* Buoyancy samples

\* Camera surface query

\* Particle/surface transition

\* Shoreline coupling



Do not automatically run it for every water fragment.



\## Phase acceptance



\* Impact markers remain aligned with visible displaced waves.

\* Foam-source markers stay attached to generating crests.

\* Camera surface query agrees with rendered height.

\* Coordinate debug views clearly distinguish (q) and (x).

\* No per-fragment inverse solver is introduced without measured justification.



\---



\# 7. Phase WTR-020 — Spectrum quality and FFT state



\## Objective



Turn the current simplified spectral generator into a controllable wind-sea and swell system without destroying phase continuity.



\## WTR-021 — Separate initial spectrum from time evolution



The current spectrum shader recalculates:



\* Gaussian random value

\* Spectral energy

\* Time evolution



in the same per-frame pass.



Split this into:



\### Initial-spectrum pass



Runs when relevant sea-state parameters change.



Stores:



\* (H\_0(k))

\* Conjugate partner data

\* Stable Gaussian random field

\* Band weights



\### Evolution pass



Runs each simulation update.



Calculates:



\* Current complex height

\* Horizontal displacement spectra

\* Velocity spectra

\* Slope spectra



Benefits:



\* Less repeated spectral work

\* Stable phase data

\* Easier weather transitions

\* Cleaner profiling

\* Better control over spectrum changes



\## WTR-022 — Proper wind-sea spectrum



Implement a selectable spectral model:



```text

Current simplified spectrum

JONSWAP

TMA for finite depth

```



For JONSWAP expose:



\* Wind speed

\* Wind direction

\* Fetch

\* Peak enhancement (\\gamma)

\* Directional spreading

\* Short-wave damping

\* Overall energy

\* Choppiness



\## WTR-023 — Horvath-style directional spreading



Add configurable directional models for:



\* Broad wind sea

\* Narrow swell

\* Opposing weak waves

\* Cross swell



Avoid one hard-coded directional expression for every sea state.



\## WTR-024 — Separate swell spectrum



Add an independent swell component:



\* Direction

\* Period or peak wavelength

\* Amplitude

\* Directional width

\* Choppiness

\* Age/decay

\* Optional secondary swell



Local wind direction must not force the direction of all long waves.



\## WTR-025 — Cascade band verification



Retain overlapping cascade bands but add tests that verify:



\* No large energy gaps

\* No excessive duplicated energy

\* Smooth transition across wavelengths

\* Sum of cascade weights remains controlled

\* Different spectrum components map predictably to cascades



\## WTR-026 — Phase-continuous weather changes



Do not replace the visible spectrum instantaneously.



For modest changes:



\* Preserve random phases

\* Interpolate spectral amplitude

\* Interpolate directional distribution

\* Give different frequency bands different response times



For major incompatible changes:



\* Simulate old and new sea states together

\* Crossfade displacement, slopes and velocity

\* Retire old state when transition completes



Suggested response hierarchy:



```text

capillary/very short waves: seconds

short wind waves: tens of seconds

medium wind waves: minutes

long swell: several minutes

```



\## Phase acceptance



\* Wind changes do not reset wave phase.

\* Swell can travel independently from local wind.

\* Calm-to-storm transition does not pop.

\* Spectral energy remains stable across cascade boundaries.

\* Current simplified mode remains available for A/B testing.



\---



\# 8. Phase WTR-030 — Frequency-aware distance filtering and ocean LOD



\## Objective



Remove distant aliasing without flattening the complete ocean.



\## WTR-031 — Replace global `wave\_fade`



Calculate visibility separately by cascade or frequency band.



Estimate projected wavelength in pixels:



```text

projectedPixels =

&#x20;   worldWavelength × pixelsPerWorldMetreAtSurface

```



Attenuate detail only when it becomes unresolved.



Expected behaviour:



\* Shortest waves disappear first

\* Medium waves persist farther

\* Long swell remains visible at the horizon

\* Horizontal displacement is filtered separately

\* Normal detail is filtered separately

\* Foam detail is filtered separately



\## WTR-032 — Separate geometry and shading visibility



Maintain separate weights:



```text

geometryWeight\[cascade]

normalWeight\[cascade]

foamWeight\[cascade]

roughnessContribution\[cascade]

```



A wave may:



\* Stop affecting geometry

\* Still affect roughness

\* Or remain in geometry while its tiny normal detail disappears



depending on projected size.



\## WTR-033 — Slope-variance roughness compensation



When unresolved small waves are filtered out:



\* Do not simply make the water perfectly smooth

\* Convert unresolved slope variance into microfacet roughness



This preserves broad highlights and prevents a distant mirror-like appearance.



\## WTR-034 — CDLOD displacement bounds



Update CDLOD node bounds so frustum and LOD selection conservatively include:



\* Maximum expected FFT crest

\* Horizontal displacement

\* Interaction displacement where relevant



Avoid disappearing crests or incorrect culling.



\## WTR-035 — Optional outer clipmap optimisation



The existing oversized CDLOD surface may already be sufficient.



Only replace or augment it with concentric clipmap rings if profiling or visible transitions justify it.



Do not implement another geometry system merely because other ocean demos use one.



\## Phase acceptance



\* Long swell remains visible at two-kilometre altitude.

\* Fine waves do not shimmer.

\* No circular cascade rings appear.

\* Distant water does not become a perfectly flat mirror.

\* Performance remains within the existing water budget.



\---



\# 9. Phase WTR-040 — Reflection ownership and cloud stability



\## Objective



Make SSR, planar geometry and sky/cloud reflection cooperate without duplicate ownership or pitch-dependent movement.



\## WTR-041 — Diagnostic separation



Render these modes separately:



1\. Directional atmosphere only

2\. Directional cloud environment only

3\. SSR only

4\. Planar sky only

5\. Planar clouds only

6\. Planar terrain only

7\. Planar objects only

8\. Full planar result

9\. Final composition



Run the cloud-pitch test with:



\* Frozen cloud time

\* Frozen wave normals

\* Fixed camera position

\* Changing camera pitch only



\## WTR-042 — Separate planar geometry validity from planar sky



The planar target should carry explicit information identifying reflected geometry.



Recommended outputs:



```text

planar colour

planar depth

geometry validity

optional material category

```



The final composition should not treat a planar sky pixel as if it were reflected terrain.



\## WTR-043 — Choose one cloud-reflection owner



Preferred production hierarchy:



```text

SSR for close visible geometry

&#x20;   ↓ fallback

planar terrain and objects

&#x20;   ↓ fallback

directional atmosphere and clouds

```



This means:



\* Planar pass renders geometry and optionally a transparent/neutral sky background

\* Directional reflected cloud evaluation provides cloud reflection

\* Planar geometry validity decides where it replaces directional sky/cloud



This is likely the most stable solution because cloud reflection becomes direction-based rather than average-plane-screen-projection-based.



Alternative experimental path:



\* Keep finite-altitude clouds in reflected-camera space

\* Remove cloud contribution from the environment map

\* Prove that this remains stable under pitch and temporal reprojection



Do not retain both full cloud sources in final production composition.



\## WTR-044 — SSR confidence



Replace one scalar hit weight with a confidence based on:



\* Edge distance

\* Hit thickness error

\* Travel distance

\* Surface roughness

\* Normal variation

\* Ray direction

\* Depth discontinuity

\* Temporal consistency



\## WTR-045 — Shared Hi-Z traversal



Build one screen-space ray-query utility that can later serve:



\* SSR

\* Refraction

\* Optional underwater surface intersection



Features:



\* Hi-Z traversal

\* Adaptive stepping

\* Thickness tolerance

\* Binary refinement

\* Edge rejection

\* Confidence result

\* Failure reason



Do not maintain independent low-quality ray marchers for reflection and refraction.



\## Phase acceptance



\* Clouds remain anchored when camera pitch changes.

\* Every reflection pixel has an identifiable principal source.

\* Planar sky cannot overwrite directional clouds unintentionally.

\* SSR transitions smoothly to planar geometry or sky.

\* No reflection layer flickers rapidly under TAA or camera motion.



\---



\# 10. Phase WTR-050 — Refraction, transparency and water optics



\## Objective



Make the displaced water surface visibly refract the seabed and underwater objects correctly.



\## WTR-051 — Shared optical parameter block



Replace isolated scalar colour controls with:



```text

index of refraction

absorption RGB

scattering RGB

deep scatter colour

turbidity

phase anisotropy

surface reflection artistic multiplier

surface reflection artistic bias

```



Use the same optical parameters for:



\* Surface water

\* Underwater compositor

\* Local splash rendering

\* God rays

\* Caustics

\* Bubbles



\## WTR-052 — Physical Fresnel foundation



For water IOR around 1.333, derive normal-incidence reflectance from IOR.



Keep artistic controls separate:



```text

physicalF0

reflectionMultiplier

reflectionBias

```



Do not hide artistic amplification inside the physical Fresnel value.



\## WTR-053 — Improved low-cost refraction



Before full ray traversal:



1\. Start from displaced surface position.

2\. Calculate physical refracted direction.

3\. Convert direction to a depth-dependent screen offset.

4\. Sample several points.

5\. Reject foreground geometry.

6\. Reduce edge distortion.

7\. Return validity/confidence.

8\. Fall back to unwarped transmission.



This becomes the medium quality mode.



\## WTR-054 — Hi-Z refracted-ray intersection



High-quality mode:



1\. Start at displaced water surface.

2\. Refract view direction using Snell’s law.

3\. Traverse Hi-Z depth.

4\. Detect first valid depth crossing.

5\. Refine hit.

6\. Calculate travelled water distance.

7\. Sample scene colour.

8\. Apply RGB absorption and scattering.

9\. Return confidence.



\## WTR-055 — Disocclusion fallback



Screen-space data cannot reveal hidden seabed.



Fallback order:



1\. Valid refracted screen-space hit

2\. Optional low-resolution underwater colour/depth render

3\. Unwarped scene transmission

4\. Probe/environment approximation

5\. Deep-water scattering colour



\## WTR-056 — Water thickness and path length



Use actual or approximated travelled distance, not only vertical depth.



Apply:



\[

T=e^{-\\sigma\_t L}

]



with RGB coefficients.



\## Phase acceptance



\* Checkerboard seabed bends under passing waves.

\* Refraction changes with view angle.

\* No foreground object is dragged beneath the water.

\* Edge failures do not smear.

\* Red attenuates sooner than blue in clear deep water.

\* Medium and high-quality modes can be compared independently.



\---



\# 11. Phase WTR-060 — Interaction solver redesign



\## Objective



Support small impacts and large wakes without multiplying current event-loop cost.



\## WTR-061 — Separate injection and propagation



Current design checks every event in every texel.



Replace with:



\### Injection pass



Writes:



\* Height impulse

\* Velocity impulse

\* Pressure

\* Aeration

\* Directional momentum



\### Propagation pass



Reads the resulting field and advances the solver.



\## WTR-062 — Event-centred injection



Dispatch work over each event’s bounded region, or bin events into tiles.



Preferred options:



\### Event bounding dispatch



Each event dispatches only over its affected rectangle.



\### Tiled event binning



1\. Divide field into tiles.

2\. Assign overlapping events to each tile.

3\. Each tile processes only local events.

4\. Propagation reads the accumulated result.



Do not increase resolution while retaining the full per-texel 48-event scan.



\## WTR-063 — Fixed simulation timestep



Use an accumulator:



```text

accumulator += frameDelta



while accumulator >= fixedStep:

&#x20;   simulate(fixedStep)

&#x20;   accumulator -= fixedStep

```



Choose the step from solver stability and cell size.



Report when overload causes simulation time to be dropped.



\## WTR-064 — Near interaction field



Initial target:



```text

physical extent: 64 × 64 m

resolution: 512 × 512

cell size: 12.5 cm

```



Used for:



\* Bullets

\* Footsteps

\* Swimming limbs

\* Small debris

\* Rain

\* Returning droplets

\* Close splash detail



These numbers are starting values, not unchangeable requirements. Profile them.



\## WTR-065 — Medium interaction field



Initial target:



```text

physical extent: 256–512 m

resolution: 256² or 512²

```



Used for:



\* Boats

\* Large vehicles

\* Explosions

\* Aircraft impacts

\* Long wakes

\* Large displaced volume



\## WTR-066 — Moving-domain history



Use:



\* Toroidal textures

\* Integer-cell shifts

\* Reprojection

\* Border clearing

\* Overlap blending



Do not reset the full field during ordinary camera movement.



\## WTR-067 — Field coupling



Final surface:



\[

H=H\_{\\mathrm{FFT}}+H\_{\\mathrm{near}}+H\_{\\mathrm{medium}}

]



Blend overlapping fields by frequency/scale so the same disturbance is not doubled.



\## Phase acceptance



\* Projectile ripples can be below 20 cm visible diameter.

\* Ripple speed changes by less than 2% between 30, 60 and 120 FPS.

\* Camera movement does not erase nearby wakes.

\* Near field no longer checks every possible event from every cell.

\* Medium wake remains stable outside the near field.



\---



\# 12. Phase WTR-070 — Disturbance and wake models



\## Bullet



Generate:



\* Small downward velocity impulse

\* Brief cavity

\* Narrow outgoing ring

\* Small aeration source

\* Optional tiny spray request



Use the projectile’s swept segment through the surface, not only its final impact point.



\## Footstep



Generate:



\* Elliptical pressure footprint

\* Heel-to-toe timing

\* Motion-direction bias

\* Different strength for walking, running and wading



\## Swimming



Use several emitters:



\* Torso pressure

\* Hands

\* Feet

\* Continuous body wake

\* Hand-entry spray

\* Underwater bubbles



\## Boat



Do not create a boat wake from repeated circular events.



Use:



\* Moving hull-pressure footprint

\* Bow displacement

\* Stern release

\* Diverging wake

\* Transverse wave

\* Propeller turbulence

\* Hull-relative velocity

\* Persistent foam trail



\## Explosion



Simulate stages:



1\. Rapid cavity

2\. Outward pressure wave

3\. Central collapse

4\. Secondary oscillation

5\. Aeration

6\. Surface foam

7\. Spray plume

8\. Bubble column for underwater energy



\## Falling object



Derive response from:



\* Impact velocity

\* Entry angle

\* Projected area

\* Approximate displaced volume

\* Mass category

\* Water depth



\## Swept emitters



Every fast emitter should provide:



```text

previous position

current position

velocity

radius

direction

energy

source type

```



Inject along the swept capsule to prevent dotted gaps.



\---



\# 13. Phase WTR-080 — Persistent foam and breaking energy



\## Objective



Make foam a transported consequence of breaking water rather than several unrelated animated patterns.



\## WTR-081 — Breaking-energy field



Combine continuous signals:



\* Crest elevation

\* Jacobian compression

\* Curvature

\* Steepness

\* Vertical velocity

\* Negative surface-velocity divergence

\* Kinetic energy

\* Local depth

\* Interaction aeration



Do not require all terms to exceed hard thresholds simultaneously.



Example concept:



\[

E\_{\\mathrm{break}} =

E\_{\\mathrm{crest}}

\\cdot

W\_{\\mathrm{compression}}

\\cdot

W\_{\\mathrm{convergence}}

\\cdot

W\_{\\mathrm{kinetic}}

\\cdot

W\_{\\mathrm{depth}}

]



Use smooth weights and artistic controls.



\## WTR-082 — Surface divergence and vorticity



Derive from surface and interaction velocity:



\[

\\nabla \\cdot v

]



\[

\\nabla \\times v

]



Negative divergence identifies converging flow and probable air entrainment.



Vorticity can assist:



\* Swirling wake foam

\* Explosion collapse

\* Shoreline turbulence

\* Whitewater motion



\## WTR-083 — Air-entrainment field



Store a temporally filtered field representing air mixed into the water.



Sources:



\* Breaking waves

\* Boat bow

\* Propeller

\* Explosion collapse

\* Object entry

\* Shore breakers

\* Interaction convergence



Uses:



\* Foam creation

\* Bubble creation

\* Spray emission

\* Local turbidity

\* Underwater scattering

\* Sound intensity



\## WTR-084 — Separate foam causes



Logical channels:



```text

breaker foam

wake/interaction foam

shore foam

age or aeration

```



They may be packed, but source and decay controls must remain separate.



\## WTR-085 — Surface-velocity advection



Advect foam using:



\[

v\_{\\mathrm{foam}}=

v\_{\\mathrm{surface}}

\+

v\_{\\mathrm{current}}

\+

k\_{\\mathrm{wind}}v\_{\\mathrm{wind}}

\+

v\_{\\mathrm{interaction}}

]



Use semi-Lagrangian backtracing.



Wind alone must no longer be the dominant transport mechanism.



\## WTR-086 — Foam ageing



Track:



\* Density

\* Age

\* Aeration

\* Thickness



Fresh foam:



\* Bright

\* Dense

\* Highly scattering



Old foam:



\* Broken

\* Thin

\* Streaked

\* Less opaque

\* More influenced by current



\## WTR-087 — Shore foam



Use:



\* Shore distance

\* Seabed slope

\* Incoming wave direction

\* Wave phase

\* Local depth

\* Swash velocity

\* Previous foam

\* Retreat velocity



Foam should advance and recede rather than hover permanently within one depth band.



\## Phase acceptance



\* Foam stays attached to the crest that created it.

\* Foam persists after the crest passes.

\* Boat foam forms a continuous trail.

\* Explosion foam has a distinct lifetime.

\* Shore foam advances and retreats.

\* Calm water remains clean.



\---



\# 14. Phase WTR-090 — Unified whitewater particles



\## Objective



Create one GPU secondary-particle system for:



\* Spray

\* Surface foam clumps

\* Bubbles

\* Droplets



Sebastian Lague’s implementation demonstrates the useful principle of deriving whitewater emission from converging relative velocity and kinetic energy, generating fractional particle counts stochastically, spawning them through a swept volume, and dynamically treating particles as spray, foam or bubbles. His complete liquid remains an SPH particle simulation; CWR-CE should adopt the whitewater ideas without replacing the FFT ocean with SPH. (\[GitHub]\[13])



\## WTR-091 — Particle structure



Conceptual GPU record:



```wgsl

struct WhitewaterParticle

{

&#x20;   position: vec3<f32>,

&#x20;   previousPosition: vec3<f32>,



&#x20;   velocity: vec3<f32>,

&#x20;   age: f32,

&#x20;   lifetime: f32,



&#x20;   radius: f32,

&#x20;   density: f32,



&#x20;   state: u32,

&#x20;   sourceType: u32,

&#x20;   sourceId: u32,

&#x20;   randomSeed: u32,

};

```



States:



```text

spray

surface foam

bubble

droplet

```



\## WTR-092 — Fractional deterministic emission



For expected count (r):



```text

guaranteed = floor(r)

fraction = r - guaranteed



spawn guaranteed

spawn one more with probability fraction

```



Use stable deterministic random seeds in capture mode.



\## WTR-093 — Swept-volume spawning



Spawn inside a capsule or cylinder from previous to current emitter position.



Applications:



\* Crest segment

\* Boat bow

\* Propeller

\* Projectile path

\* Falling object

\* Explosion plume

\* Shore breaking front



\## WTR-094 — State classification



Use signed distance to the combined water surface:



```text

above surface band → spray

near interface → foam

below interface → bubble

```



Refine using:



\* Vertical velocity

\* Aeration

\* Water depth

\* Local normal

\* Particle density

\* Source type



Do not rely on SPH fluid-neighbour count because the primary ocean is not particle-based.



\## WTR-095 — State physics



\### Spray



\* Gravity

\* Wind

\* Quadratic drag

\* Scene collision

\* Surface impact transition



\### Surface foam particle



\* Constrained toward water surface

\* Advected by water velocity

\* Small inertial lag

\* Wind coupling

\* Deposits persistent foam



\### Bubble



\* Buoyancy

\* Drag toward water velocity

\* Current and turbulence

\* Transfers aeration to surface foam when it reaches the interface



\## WTR-096 — GPU compaction



Use ping-pong particle buffers:



```text

active input

&#x20;   ↓ update and survivor compaction

active output

&#x20;   ↓ swap

next frame

```



Generate indirect draw arguments on GPU.



No per-frame CPU particle readback.



\## WTR-097 — Pool priority



Reserve or prioritise capacity:



```text

highest:

&#x20;   nearby explosion

&#x20;   player projectile

&#x20;   player vehicle



medium:

&#x20;   nearby breakers

&#x20;   shoreline

&#x20;   nearby AI vehicles



lower:

&#x20;   distant whitecaps

&#x20;   distant wakes

```



Expose:



\* Active count

\* Spawned count

\* Dropped count

\* Count by state

\* Count by source



\## WTR-098 — Particle/foam coupling



Particles deposit into persistent foam.



Persistent high-energy breaker regions can emit particles.



The systems must look like two scales of one process, not unrelated effects.



\## Phase acceptance



A single particle can naturally transition:



```text

spray

→ surface foam

→ submerged bubble

→ rising bubble

→ surface foam

→ expiry

```



Boat spray is continuous.



Explosion creates spray, foam and underwater bubbles.



Pool overload degrades gracefully.



\---



\# 15. Phase WTR-100 — Underwater classification and waterline



\## Objective



Replace the current depth-and-camera-direction heuristic with the actual displaced surface.



\## WTR-101 — Camera surface query



Evaluate:



\[

d=y\_{\\mathrm{camera}}-H(x,z,t)

]



Use the combined surface:



\* FFT

\* Near interaction

\* Medium interaction



\## WTR-102 — Hysteresis



```text

enter underwater below -enterThreshold

exit underwater above +exitThreshold

```



Avoid rapid toggling near the surface.



\## WTR-103 — Waterline rendering



The visible waterline and compositor transition must use the same displaced-surface evaluation.



Possible implementation:



\* Surface intersection mask

\* Stencil

\* Dedicated waterline depth

\* Camera signed distance plus per-pixel surface intersection



Do not determine submersion from whether the player is looking down.



\## Phase acceptance



\* Passing crest submerges a stationary camera.

\* Looking downward does not activate underwater mode.

\* Waterline does not flicker.

\* Surface and compositor agree on the wave location.



\---



\# 16. Phase WTR-110 — Underwater optics



\## Objective



Replace the current procedural full-screen colour effect with shared physical water optics.



\## WTR-111 — Expanded underwater parameters



Supply:



\* Inverse view-projection

\* Camera position

\* Sun direction

\* Sun radiance

\* Moon/night factor

\* Water absorption RGB

\* Water scattering RGB

\* Anisotropy

\* Turbidity

\* Sea level

\* Camera depth below displaced surface

\* Water-body type

\* Whitewater aeration

\* Current

\* Surface displacement/normal resources

\* Terrain shadow mask

\* CSM resources

\* Underwater froxel volume



\## WTR-112 — Correct depth reconstruction



Replace `0.12/depth` approximation with camera-relative position reconstruction through the inverse view-projection.



Use actual path distance from camera to opaque scene surface.



\## WTR-113 — Common RGB extinction



Apply the same optical coefficients as the surface shader.



Compute:



```text

transmitted scene

\+

integrated in-scattering

```



Do not apply a separate hard-coded underwater colour unrelated to surface water.



\## WTR-114 — Underwater distortion



Replace the two screen-space sine waves with a subtle combination of:



\* FFT surface transmission

\* Low-frequency current

\* Local interaction field

\* Heat-haze-like refractive variation from suspended water motion



Keep distortion subtle and depth-aware.



Objects close to the camera should distort less than distant objects.



\## WTR-115 — Suspended particulate matter



Add world-anchored particles or low-frequency volumetric density variation.



Use it to make shafts visible without filling the image with static noise.



Particulates should:



\* Move with current

\* Vary by turbidity

\* Increase near shore

\* Increase near explosions and wakes

\* Fade in clear deep ocean water



\---



\# 17. Phase WTR-120 — Underwater god rays



\## Objective



Create physically coherent underwater light shafts using the renderer’s existing volumetric and shadow infrastructure.



\## 17.1 Architectural decision



Do not implement god rays as a radial blur from the screen-space sun position.



The renderer already contains a frustum-aligned atmospheric froxel system with:



\* 3D in-scattering and transmittance

\* Terrain shadow mask

\* CSM binding infrastructure

\* Front-to-back accumulation

\* God-ray-oriented shadow logic. (\[GitHub]\[14])



Create a separate \*\*underwater froxel volume\*\* using the same structural approach.



Do not reuse atmospheric density functions.



\## WTR-121 — Refactor reusable froxel infrastructure



Extract reusable components where practical:



\* 3D froxel target creation

\* Frustum slice mapping

\* Shadow bindings

\* Terrain-shadow sampling

\* CSM sampling

\* Trilinear sampling helper

\* Resource resize/rebuild



Do not merge atmospheric and underwater medium equations into one unreadable shader.



\## WTR-122 — Underwater froxel medium



Suggested initial resolution:



```text

32 × 32 × 32

```



Quality modes may use:



```text

low:    24 × 14 × 24

medium: 32 × 18 × 32

high:   48 × 27 × 40

```



Use a maximum underwater range based on water clarity, for example:



\* Clear: 100–150 metres

\* Coastal: 40–80 metres

\* Turbid: 15–40 metres



Slice distribution should be denser near the camera.



\## WTR-123 — Single scattering



For each froxel segment:



1\. Determine local water extinction.

2\. Determine scattering coefficient.

3\. Evaluate phase function toward the camera.

4\. Evaluate sunlight arriving from the surface.

5\. Apply terrain/object shadow visibility.

6\. Apply water-surface transmission.

7\. Integrate in-scattering and transmittance.



Use a configurable Henyey–Greenstein phase function.



Forward scattering should be stronger when looking approximately toward the incoming sunlight.



\## WTR-124 — Surface-light transmission



Sunlight entering water depends on:



\* Sun direction

\* Surface normal

\* Fresnel transmission

\* Surface shadow/cloud transmittance

\* Water surface displacement

\* Refraction direction



For initial implementation:



\* Sample a filtered FFT surface normal

\* Refract sun direction into water

\* Apply Fresnel transmission

\* Use cloud/sky sun visibility where available



Later improve with a dedicated surface-light transmission map.



\## WTR-125 — Terrain and object occlusion



Use:



\* Long-range terrain shadow mask

\* CSM for nearby terrain, trees, buildings and vehicles



Unlike the current atmospheric froxel code, underwater shafts occur close enough that CSM may be important.



Test whether current CSM range covers the underwater volume. Extend or add an underwater-specific range only if necessary.



\## WTR-126 — Cloud modulation



Clouds should reduce underwater sunlight and god-ray intensity.



Do not render bright underwater shafts beneath solid overcast.



Use the sky/cloud system’s sunlight transmittance or a dedicated scalar passed to underwater lighting.



\## WTR-127 — Temporal stability



Use:



\* Jittered froxel integration

\* Temporal accumulation

\* Camera-motion reprojection

\* History rejection on depth and submersion changes

\* Clamp to neighbourhood range



Avoid banding and flicker.



\## WTR-128 — Composite



The underwater compositor samples:



```text

froxel in-scattering

froxel transmittance

scene colour

scene depth

```



Final result:



\[

C=C\_{\\mathrm{scene}}T+L\_{\\mathrm{scatter}}

]



Do not apply a second unrelated haze after the froxel result.



\## Phase acceptance



\* Shafts appear only with sufficient scattering.

\* Terrain and buildings block shafts.

\* Open gaps between trees and ridges create shafts.

\* Overcast reduces shafts.

\* Night removes sunlight shafts.

\* Clear water does not become artificially foggy.

\* Turbid water has stronger but shorter-range shafts.

\* Camera movement produces no obvious 3D-grid slicing.



\---



\# 18. Phase WTR-130 — Surface-derived caustics



\## Objective



Replace the current screen-space sine multiplier with caustics derived from the actual wave surface.



\## WTR-131 — Initial screen-space caustic projection



For visible underwater receivers:



1\. Reconstruct world position.

2\. Determine water-surface intersection above it.

3\. Sample FFT surface derivatives.

4\. Refract sun rays.

5\. Estimate focusing/compression.

6\. Apply receiver normal and sun shadow.

7\. Attenuate with water depth.



Use this as the first production implementation.



\## WTR-132 — Caustic intensity source



Use the Jacobian of the refracted-light mapping, not arbitrary animated noise.



Approximate intensity from:



\* Surface curvature

\* Refracted-ray convergence

\* Wave slope

\* Sun angle

\* Water depth



Clamp aggressively to avoid explosive highlights.



\## WTR-133 — Shadow and depth rejection



Caustics must not:



\* Appear on surfaces outside water

\* Shine through terrain

\* Appear on the back side of objects

\* Remain strong at great depth

\* Appear at night

\* Ignore cloud shadow



\## WTR-134 — Temporal filtering



Caustics move rapidly.



Use:



\* World-space anchoring

\* Blue-noise/jitter

\* Temporal accumulation

\* Velocity-aware rejection

\* Depth/normal rejection



\## WTR-135 — Higher-quality light-space caustics



Only after initial screen-space caustics work:



\* Render a light-space water-surface map

\* Refract sample rays

\* Splat focused energy into a caustic texture

\* Project onto underwater receivers



Profile before adoption.



\## Phase acceptance



\* Caustics follow the real FFT waves.

\* Patterns change with sun direction.

\* Patterns weaken with depth.

\* Cloud and terrain shadows suppress them.

\* No caustics appear above water.

\* The old sine multiplier is removed after comparison.



\---



\# 19. Phase WTR-140 — Underwater bubbles and local aeration



\## Objective



Connect surface whitewater to underwater visibility.



\## WTR-141 — Bubble rendering



Render underwater whitewater particles with:



\* Size-dependent highlight

\* Depth fading

\* Soft intersection

\* Refraction approximation

\* Buoyancy motion

\* Current coupling



\## WTR-142 — Bubble scattering contribution



High bubble density should:



\* Increase local scattering

\* Reduce visibility

\* Brighten near-surface aeration

\* Modulate god rays

\* Affect explosion and propeller regions



Avoid one bubble particle causing a large volumetric light change.



Deposit a filtered aeration field.



\## WTR-143 — Sources



\* Propellers

\* Underwater explosions

\* Falling objects

\* Swimmers

\* Breaking waves

\* Returning spray

\* Boat hull turbulence



\## Phase acceptance



\* Propeller creates a coherent bubble trail.

\* Underwater explosion creates a rising bubble column.

\* Bubbles reach the surface and contribute foam.

\* Aerated regions scatter more light.

\* Bubble rendering remains stable at distance.



\---



\# 20. Phase WTR-150 — Optional local volumetric fluid rendering



\## Objective



Represent dense hero splashes that cannot be expressed by a heightfield.



Do not replace the FFT ocean or the standard interaction solver with SPH.



Sebastian Lague’s main simulation performs neighbour searches, density, pressure, viscosity and particle integration. That is suitable for bounded fluid volumes, not kilometres of ocean. (\[GitHub]\[15])



\## WTR-151 — Screen-space fluid rendering prototype



For dense local splash particles:



1\. Render sphere depth.

2\. Render approximate thickness.

3\. Apply edge-preserving depth smoothing.

4\. Reconstruct position and normal.

5\. Shade with common water optics.

6\. Write depth and composite with scene.



NVIDIA’s screen-space method renders particle depth, smooths it, reconstructs normals and position, and uses thickness to approximate volume absorption. It produces only the frontmost visible surface, so it should remain a local splash technique rather than the ocean renderer. (\[NVIDIA Entwicklerdownload]\[16])



\## WTR-152 — Ocean/local-fluid fusion



Within a local replacement mask:



\* Suppress or attenuate FFT surface

\* Use local particle-fluid depth

\* Evaluate reflection only once

\* Evaluate refraction only once

\* Feather boundary in world-space



\## WTR-153 — Optional bounded SPH patch



Research only for:



\* Large nearby explosion

\* Aircraft water impact

\* Large object entry

\* Hero bow splash

\* Cinematic event



Suggested bounds:



```text

8–20 m physical extent

1–5 second lifetime

1–4 concurrent patches

quality-dependent particle count

```



\## WTR-154 — Go/no-go gate



Do not ship local SPH unless it provides:



\* Clear benefit over heightfield plus whitewater particles

\* Stable integration

\* Acceptable GPU cost

\* No visible seam

\* No gameplay dependency



The water system must remain correct with local SPH disabled.



\---



\# 21. Phase WTR-160 — Weather and water-body integration



\## WTR-161 — Weather input



Replace the fixed C++ wind values with renderer-facing weather data:



\* Wind direction

\* Wind speed

\* Gust strength

\* Overcast

\* Rain

\* Storm state

\* Optional swell state



\## WTR-162 — Frequency-dependent weather response



Short waves respond quickly.



Long swell responds slowly.



Foam and whitewater react to the evolved waves, not directly to a weather slider.



\## WTR-163 — Water-body profiles



Do not force every body of water to use the ocean FFT.



Profiles:



\### Ocean



\* FFT wind sea and swell

\* Long-range CDLOD

\* Whitecaps

\* Large wakes



\### Lake



\* Limited fetch

\* Smaller waves

\* Strong reflections

\* Shore response

\* Minimal swell



\### River



\* Directed current

\* Shallow flow

\* Anisotropic ripple propagation

\* Rapids

\* Separate mesh or spline domain



\### Small finite water



\* Local heightfield

\* Boundary reflection

\* Pressure disturbances

\* No global FFT requirement



\## WTR-164 — Coastal depth transformation



Later module using:



\* Bathymetry

\* Shore distance

\* Finite-depth dispersion

\* Wave shoaling

\* Direction refraction

\* Breaking threshold



Do not apply one global shallow-water depth to the entire ocean.



\---



\# 22. Phase WTR-170 — Gameplay, buoyancy and physics



\## Objective



Make gameplay and rendering agree about the water surface.



\## WTR-171 — Public water query



```cpp

WaterQueryResult SampleWaterSurface(

&#x20;   WaterBodyId body,

&#x20;   Vector2 worldXZ,

&#x20;   double simulationTime);

```



Return:



```text

height

normal

horizontal displacement

surface velocity

interaction height

interaction velocity

foam

aeration

water depth

water-body type

```



\## WTR-172 — CPU approximation



Implement a CPU query using:



\* Same spectrum parameters

\* Same random seeds

\* Reduced set of important wave components

\* Interaction samples where available



Avoid synchronous GPU readback in gameplay-critical code.



\## WTR-173 — GPU validation



Use asynchronous GPU samples to compare CPU and GPU surface results.



Record divergence.



\## WTR-174 — Buoyancy



Sample several points on a hull.



Calculate:



\* Buoyant force

\* Wave-relative damping

\* Horizontal drag

\* Rotational torque

\* Surface velocity

\* Current



Use the same hull intersections for wake emitters.



\## WTR-175 — Impact positioning



Convert collision/projectile positions through the shared world-to-material mapping before inserting into the near field.



\## Phase acceptance



\* Boats float on visible waves.

\* Camera and rendering agree.

\* Wake originates at hull-water intersection.

\* CPU/GPU height difference stays within the defined tolerance.

\* Gameplay remains functional with FFT disabled.



\---



\# 23. Phase WTR-180 — FFT and GPU optimisation



\## Objective



Optimise only after profiling proves where the cost lies.



\## Candidate WTR-181 — Persisted initial spectrum



Likely high-value because the current spectrum calculation recreates random and energy terms every update.



\## Candidate WTR-182 — Precomputed butterfly/twiddle data



Remove repeated:



\* Bit reversal

\* Sine

\* Cosine



from every FFT stage.



\## Candidate WTR-183 — Stockham FFT



Prototype a Stockham transform:



\* Regular access

\* No explicit bit reversal

\* Potentially cleaner GPU scheduling



Keep the existing FFT for comparison.



\## Candidate WTR-184 — Tiled transpose



Use workgroup memory if profiling shows bandwidth and dispatch cost justify it.



\## Candidate WTR-185 — Cascade update scheduling



Possible update rates:



```text

short cascade: every frame

medium cascades: every frame or alternating

longest cascade: reduced rate with interpolation

```



Do not allow visible phase stepping.



\## Candidate WTR-186 — Precision review



Profile whether all intermediate packs require RGBA32F.



Possible reductions must pass numerical and visual tests.



\## Optimisation acceptance



\* Measured improvement on minimum and target GPU

\* No visible phase error

\* No new shimmer

\* No cascade stepping

\* Memory change documented

\* Dispatch change documented



\---



\# 24. Recommended implementation order



Follow this order:



```text

1\. WTR-000  Instrumentation and tests



2\. WTR-010  Shared surface state

3\. WTR-030  Frequency-aware distance filtering



4\. WTR-040  Reflection ownership

5\. WTR-050  Refraction and RGB optics



6\. WTR-060  Interaction architecture

7\. WTR-070  Disturbance and wake models



8\. WTR-080  Foam transport and breaking energy

9\. WTR-090  Unified whitewater particles



10\. WTR-100 Underwater classification

11\. WTR-110 Underwater optics

12\. WTR-120 Underwater god rays

13\. WTR-130 Surface-derived caustics

14\. WTR-140 Bubbles and aeration



15\. WTR-020 Spectrum quality and transitions

16\. WTR-160 Weather and water bodies

17\. WTR-170 Gameplay and buoyancy



18\. WTR-150 Optional local volumetric splashes

19\. WTR-180 FFT optimisation

```



The spectrum phase may be developed earlier in a separate branch after instrumentation, but it should not block correcting the most visible current issues:



\* Full distant flattening

\* Cloud reflection ownership

\* Refraction

\* Interaction resolution

\* Foam transport

\* Underwater lighting



\---



\# 25. Agent work-package rules



Every agent receives exactly one task ID.



\## Required task format



```text

Task ID:

Single objective:



Current verified behaviour:

Relevant files:

Relevant call path:



Required implementation:

1\.

2\.

3\.



Must preserve:

\- Gerstner fallback

\- Existing public FFI unless task explicitly changes it

\- Existing HDR and LDR paths

\- Existing water controls

\- Existing tests

\- Existing interaction producers



Forbidden:

\- Unrelated cleanup

\- Parameter retuning outside task

\- New external dependencies

\- Duplicate implementation

\- Silent failure



Required debug output:



Test scenes:

\- WTR-Test-...



Acceptance criteria:

\- Visual:

\- Numerical:

\- Performance:



Deliverables:

\- Code

\- Tests

\- Before/after captures

\- GPU timings

\- Known limitations

```



\## Required agent preflight



Before changing code, the agent must:



1\. Build the unmodified target.

2\. Read all named files completely.

3\. Search all callers.

4\. Trace C++→FFI→Rust→shader resources.

5\. Identify resource creation and resize.

6\. Identify MSAA/HDR/LDR differences.

7\. Identify fallback behaviour.

8\. State assumptions.

9\. Stop if any critical operation is unverified.



\## Required final report



```text

Task ID

Objective

Files read

Call path verified

Files changed

Implementation

Tests added

Build command

Build result

Runtime verification

GPU timing before

GPU timing after

Known limitations

Unverified assumptions

Next dependency

```



\---



\# 26. Master instruction for AI agents



```text

You are modifying the CWR-CE new-renderer-infrastructure branch.



The current project already has a four-cascade FFT ocean, Gerstner

fallback, CDLOD water geometry, persistent foam, a camera-relative

interaction field, SSR, reflected-camera planar reflections, sky and

cloud reflections, screen-space refraction and an underwater compositor.



Do not replace these systems unless the assigned task explicitly requires

a measured replacement.



Implement only the supplied WTR task ID.



Before editing:



1\. Build the relevant unmodified target.

2\. Read every named source file completely.

3\. Search all callers and FFI entry points.

4\. Trace resource creation, binding, update and lifetime.

5\. Identify HDR, LDR, MSAA and fallback behaviour.

6\. Identify existing tests and coding conventions.

7\. Write a short verified call graph.

8\. State anything that remains uncertain.



Rules:



\- Do not invent engine APIs.

\- Do not add unrelated procedural noise to hide a structural problem.

\- Do not combine multiple water milestones.

\- Do not remove Gerstner fallback.

\- Do not silently alter existing Water tab parameter meanings.

\- Do not let C++, Rust and WGSL struct layouts diverge.

\- Add compile-time size checks where FFI structures change.

\- Add WGSL validation tests for new shaders.

\- Add deterministic debug output.

\- Add GPU timestamps before claiming an optimisation.

\- Preserve old behaviour behind an A/B option until the new path passes.

\- Use the same optical parameters across surface, underwater and local fluid.

\- Use the same authoritative surface data for rendering and gameplay.

\- Do not replace the ocean with SPH.

\- Do not use screen-space fluid reconstruction for the main ocean.

\- Do not increase interaction resolution while retaining a full

&#x20; texel-by-event scan.

\- Do not implement god rays as a simple radial blur.

\- Do not use arbitrary screen-space sine patterns as final caustics.



If an exact required operation cannot be verified, stop and report:



\- Missing operation

\- Files and symbols inspected

\- Closest existing implementation

\- Why continuing would require an assumption

\- Smallest discovery task required



Final report:



Task ID

Files read

Verified call graph

Files changed

Implementation

Validation

Tests

Build result

Runtime result

GPU timing

Known limitations

Unverified assumptions

Next dependency

```



\---



\# 27. Final definition of done



The water system is production-ready when all the following are true.



\## Ocean



\* Wind sea and swell are independently controllable.

\* Weather transitions preserve phase.

\* Long swell remains visible at the horizon.

\* Fine waves filter without shimmer.

\* No obvious cascade seams or tiling appear.

\* Gerstner fallback remains functional.



\## Reflections



\* Clouds remain stable during camera pitch.

\* SSR, planar geometry and directional sky/cloud have explicit ownership.

\* Planar invalid pixels cannot replace the environment.

\* Reflection confidence is stable.

\* Roughness filtering works at distance.



\## Refraction



\* Seabed visibly bends under displaced waves.

\* Foreground leaking is controlled.

\* RGB absorption depends on travelled distance.

\* Edge failures fall back safely.

\* Surface and underwater optics use the same parameters.



\## Interaction



\* Bullets produce centimetre-scale ripples.

\* Footsteps are directional.

\* Swimming creates wakes, spray and bubbles.

\* Boat wakes are continuous.

\* Explosions produce cavity, collapse, ring, foam, spray and bubbles.

\* Simulation speed is framerate independent.

\* Camera movement preserves interaction history.



\## Foam and whitewater



\* Foam is advected by actual water motion.

\* Breaker, wake and shore foam remain distinguishable.

\* Foam remains attached to generating features.

\* Spray, foam and bubbles transition naturally.

\* Particle overload degrades gracefully.

\* Calm water stays clean.

\* Distant foam remains stable.



\## Underwater



\* Camera submersion follows displaced water height.

\* Waterline does not flicker.

\* Underwater colour comes from RGB optical coefficients.

\* God rays are volumetric and shadowed.

\* Terrain, buildings and trees block sunlight.

\* Clouds reduce underwater illumination.

\* Caustics derive from the real surface.

\* Bubbles increase local aeration and scattering.

\* Night and overcast conditions behave correctly.



\## Gameplay



\* Camera, buoyancy and rendering agree on water height.

\* Boats respond to visible waves.

\* Wakes originate at real hull intersections.

\* Impacts align with displaced surface positions.

\* CPU and GPU query divergence is measured.



\## Performance



\* Every major pass has GPU timing.

\* Quality presets are documented.

\* Expensive features can be disabled independently.

\* Optional local fluid is not required for gameplay.

\* FFT optimisation is supported by measurements.

\* Minimum and target GPU budgets are documented.



\---



\# 28. Final technical direction



The current implementation has most of the correct large pieces.



The best path is not to discard them. It is to make them share:



\* One surface definition

\* One velocity definition

\* One optical model

\* One breaking-energy model

\* One reflection-ownership hierarchy

\* One interaction coordinate system

\* One underwater-lighting model



The completed architecture should behave as one water simulation:



```text

FFT waves create the sea



interaction fields let actors disturb it



breaking energy determines where coherent water becomes whitewater



persistent foam preserves broad surface coverage



particles carry spray, foam and bubbles



reflections and refraction use the displaced surface



underwater froxels carry sunlight, shadow and scattering



gameplay queries sample the same result

```



That is the route from the water currently on GitHub to a system capable of exceeding the individual inspiration projects in overall coherence and feature depth.



\[1]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/WaterWgpu.cpp "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/WaterWgpu.cpp"

\[2]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/WaterWgpu.hpp "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/WaterWgpu.hpp"

\[3]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/water.wgsl "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/water.wgsl"

\[4]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/fft.rs "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/fft.rs"

\[5]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/fft\_stage.wgsl "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/fft\_stage.wgsl"

\[6]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/fft\_spectrum.wgsl "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/fft\_spectrum.wgsl"

\[7]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/fft\_compose.wgsl "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/fft\_compose.wgsl"

\[8]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/interaction.wgsl "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/interaction.wgsl"

\[9]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/foam.wgsl "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/water/foam.wgsl"

\[10]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/lib.rs "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/lib.rs"

\[11]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/sky/mod.rs "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/sky/mod.rs"

\[12]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/underwater.wgsl "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/underwater.wgsl"

\[13]: https://github.com/SebLague/Fluid-Sim "https://github.com/SebLague/Fluid-Sim"

\[14]: https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/sky/sky.wgsl "https://raw.githubusercontent.com/koosoli/CWR-CE-paavohuhtalafork-/new-renderer-infrastructure/engine/WgpuRenderer/rust/src/sky/sky.wgsl"

\[15]: https://raw.githubusercontent.com/SebLague/Fluid-Sim/main/Assets/Scripts/Simulation/Compute/FluidSim.compute "https://raw.githubusercontent.com/SebLague/Fluid-Sim/main/Assets/Scripts/Simulation/Compute/FluidSim.compute"

\[16]: https://developer.download.nvidia.com/presentations/2010/gdc/Direct3D\_Effects.pdf "https://developer.download.nvidia.com/presentations/2010/gdc/Direct3D\_Effects.pdf"















**Also I would like that the more deep the water goes the less translucent and deeper colored it will be, does your plan account for that?** 



**Yes. The plan accounts for that in WTR-050 Refraction and Water Optics and WTR-110 Underwater Optics, but I would make the requirement more explicit for the agents.**



**The water should not become darker merely by increasing opacity according to vertical seabed depth. It should use the distance light actually travels through the water:**



**T**

**RGB**

&#x09;**​**



**=e**

**−σ**

**extinction,RGB**

&#x09;**​**



**L**



**Where:**



**L is the optical path through the water**

**σ**

**extinction,RGB**

&#x09;**​**



&#x20;**controls how quickly red, green and blue disappear**

**Red normally disappears first, then green, while blue penetrates farther**



**That produces the desired progression:**



**Very shallow water**

**→ seabed clearly visible**

**→ weak water tint**

**→ high transmission**



**Medium depth**

**→ seabed increasingly tinted**

**→ lower contrast**

**→ stronger blue-green in-scattering**



**Deep water**

**→ seabed barely visible or invisible**

**→ transmission approaches zero**

**→ deep body colour dominates**

**What the agents should implement**



**The surface shader should calculate:**



**transmittedSeabed =**

&#x20;   **refractedSceneColour × RGBTransmittance**



**waterBodyColour =**

&#x20;   **scatteringColour × (1 − RGBTransmittance)**



**finalTransmission =**

&#x20;   **transmittedSeabed + waterBodyColour**



**This means the deep colour emerges naturally as the seabed light is absorbed, rather than being painted over it using a flat alpha.**



**The shader needs these separate controls:**



**absorptionRGB**

**scatteringRGB**

**deepScatterColour**

**turbidity**

**maximumOpticalDepth**

**minimumTransmission**



**For example:**



**Clear tropical water: low absorption, turquoise scattering**

**Deep ocean: stronger red/green absorption, dark blue scattering**

**Coastal water: higher scattering and turbidity, green-brown depth colour**

**Muddy river: rapid transmission loss over only a few metres**

**Important distinction**



**The implementation should use both:**



**Water-column/geometric depth**

**Distance from the water surface to the seabed or underwater object.**

**Optical path length**

**Distance travelled by the refracted viewing ray through the water.**



**At a shallow viewing angle, light travels through much more water than when looking straight down. Therefore, the same seabed should appear darker and more deeply coloured near the horizon.**



**When no seabed is visible**



**When the refracted ray:**



**Does not find the seabed,**

**Leaves the screen,**

**Exceeds the maximum visibility distance,**

**Or has almost zero transmission,**



**the shader should smoothly fall back to:**



**deepScatterColour**

**+ reflected light**

**+ sun glints**

**+ foam where present**



**It must not expose the undisplaced or unwarped seabed.**



**Explicit acceptance criteria to add**

**At one metre depth, the seabed remains clearly visible in clean water.**

**At increasing depths, contrast and saturation of the seabed decrease continuously.**

**Red components attenuate faster than blue.**

**Deep water becomes effectively opaque without an abrupt cutoff.**

**Looking at an angle makes the water less transparent than looking vertically downward.**

**Turbidity changes visibility distance independently from the artistic deep-water colour.**

**No flat alpha blend or fixed depth-colour gradient is used as the primary model.**



**So yes—the architecture already includes it, but this should be stated as a mandatory optical-depth implementation, not merely a colour-tint setting.**



