// Chroma key (builtin chroma_key@1). Straight alpha throughout: keying rewrites only `a` and
// spill suppression only `rgb`. Distance is Euclidean in RGB; alpha ramps from 0 at `tolerance`
// to 1 at `tolerance + softness`. Operations match the former compose/effects/chroma_key.wgsl.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let c = textureSample(src, samp_linear, in.uv);
    let key = vec3f(params.key_r, params.key_g, params.key_b);
    let dist = length(c.rgb - key);
    let soft = max(params.softness, 1e-4);
    var alpha = c.a;
    if (dist <= params.tolerance) {
        alpha = 0.0;
    } else if (dist < params.tolerance + soft) {
        alpha = c.a * ((dist - params.tolerance) / soft);
    }

    // Spill suppression: remove the part of the pixel's chroma (RGB minus its mean) that points
    // toward the key colour's chroma direction. 0 = off.
    var rgb = c.rgb;
    if (params.spill > 0.0) {
        let kc = key - vec3<f32>((key.x + key.y + key.z) / 3.0);
        let kl = length(kc);
        if (kl > 1e-4) {
            let dir = kc / kl;
            let pc = rgb - vec3<f32>((rgb.x + rgb.y + rgb.z) / 3.0);
            let along = max(dot(pc, dir), 0.0);
            rgb = clamp(rgb - dir * along * params.spill, vec3<f32>(0.0), vec3<f32>(1.0));
        }
    }
    return vec4f(rgb, alpha);
}
