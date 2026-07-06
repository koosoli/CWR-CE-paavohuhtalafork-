#define_import_path color

// sRGB <-> linear helpers for the HDR path (docs/hdr-pipeline-plan.md §5). The
// gamma-naive LDR path never calls these (the `linear` pipeline override gates
// every use), so GL33 / LDR-direct output is byte-for-byte unchanged.
//
// Proper piecewise sRGB, not a bare pow(x, 2.2): the toe matters for the dark
// ambient/shadow values a mil-sim spends most of its night budget in.

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}
