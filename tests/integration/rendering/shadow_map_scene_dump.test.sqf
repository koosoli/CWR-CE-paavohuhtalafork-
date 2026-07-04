// Cascaded shadow maps: enabling the depth pass renders visible casters into
// the cascade array on the current backend, and the map reads back non-empty
// (DumpShadowMap). The probe cross-checks the backend depth raster against
// the ShadowMath CPU reference.

triEnableShadowMaps
triWaitFrames 10
triShadowSceneDump "shadow_scene"
triShadowDepthProbe 256
triEndTest
