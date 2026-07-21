# Water Interaction Emitters

`SubmitWaterInteraction` in `Poseidon/Graphics/Rendering/WaterInteractionBridge.hpp` is the public,
thread-safe simulation-to-render submission point. It has a fixed 48-record queue matching the
`WgrWaterInteractionEvent` ABI. `WaterWgpu::DrawWater` is the sole consumer and sole caller of the
renderer ABI.

Current emitters:

- Local/player-controlled infantry: water entry and continuous wading/swimming wakes, sampled after
  existing ground-water collision resolution. The event path does not modify movement, free-fall,
  contacts, or move queues.
- `Landscape::ExplosionDammageEffects`: water-qualified non-explosive impacts emit bullet ripples;
  explosive impacts emit explosion ripples.

The Rust interaction owner retains events, stamps unset creation times using renderer time, expires
them by their lifetimes, and refreshes nearby continuous wakes in place. Its active ring is fixed at
48 records; zero active records leave the interaction compute output unchanged from its no-event
simulation path.

Splash spray is deferred. `interaction_spray.wgsl` is not integrated because the current transparent
water pass has no independently depth-tested particle/instance path or safe ordering contract with
the water surface. The ripple and persistent-foam field remains the reliable visible output. Falling
object water contact is also deferred until a single post-collision object callback can provide both
the impact position and velocity without adding a second world traversal.
