// W3C compositing and blending (https://www.w3.org/TR/compositing-1/), on premultiplied
// backdrops. Concatenated in front of composite.wgsl and effects/adjust_mix.wgsl; keep the
// mode numbering in sync with `BlendMode::shader_index` in project/effects.rs.
//
//  0 normal      1 multiply     2 screen      3 overlay     4 darken      5 lighten
//  6 color_dodge 7 color_burn   8 soft_light  9 hard_light 10 difference 11 exclusion
// 12 add (plus-lighter: the Porter-Duff "plus" operator, not a separable blend function)

fn blend_dodge(cb: f32, cs: f32) -> f32 {
    if (cb <= 0.0) { return 0.0; }
    if (cs >= 1.0) { return 1.0; }
    return min(1.0, cb / (1.0 - cs));
}

fn blend_burn(cb: f32, cs: f32) -> f32 {
    if (cb >= 1.0) { return 1.0; }
    if (cs <= 0.0) { return 0.0; }
    return 1.0 - min(1.0, (1.0 - cb) / cs);
}

fn blend_hard_light(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return cb * (2.0 * cs);
    }
    let s = 2.0 * cs - 1.0;
    return cb + s - cb * s;
}

fn blend_soft_light(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
    }
    var d: f32;
    if (cb <= 0.25) {
        d = ((16.0 * cb - 12.0) * cb + 4.0) * cb;
    } else {
        d = sqrt(cb);
    }
    return cb + (2.0 * cs - 1.0) * (d - cb);
}

// Separable blend function B(cb, cs) for one channel of un-premultiplied colour.
fn blend_channel(mode: u32, cb: f32, cs: f32) -> f32 {
    switch (mode) {
        case 1u: { return cb * cs; }
        case 2u: { return cb + cs - cb * cs; }
        case 3u: { return blend_hard_light(cs, cb); }
        case 4u: { return min(cb, cs); }
        case 5u: { return max(cb, cs); }
        case 6u: { return blend_dodge(cb, cs); }
        case 7u: { return blend_burn(cb, cs); }
        case 8u: { return blend_soft_light(cb, cs); }
        case 9u: { return blend_hard_light(cb, cs); }
        case 10u: { return abs(cb - cs); }
        case 11u: { return cb + cs - 2.0 * cb * cs; }
        default: { return cs; }
    }
}

// Source colour `cs` (straight) with alpha `a_s`, composited over the premultiplied backdrop
// `backdrop` with blend `mode`. Returns the premultiplied result.
//   co = cs*as*(1-ab) + cb_p*(1-as) + as*ab*B(Cb, Cs)       ao = as + ab*(1-as)
fn blend_over(mode: u32, backdrop: vec4<f32>, cs: vec3<f32>, a_s: f32) -> vec4<f32> {
    let ab = backdrop.a;
    if (mode == 12u) {
        return min(vec4<f32>(1.0), vec4<f32>(backdrop.rgb + cs * a_s, ab + a_s));
    }
    var cb = vec3<f32>(0.0);
    if (ab > 0.0) {
        cb = clamp(backdrop.rgb / ab, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let b = vec3<f32>(
        blend_channel(mode, cb.r, cs.r),
        blend_channel(mode, cb.g, cs.g),
        blend_channel(mode, cb.b, cs.b),
    );
    let co = cs * (a_s * (1.0 - ab)) + backdrop.rgb * (1.0 - a_s) + b * (a_s * ab);
    return vec4<f32>(co, a_s + ab * (1.0 - a_s));
}
