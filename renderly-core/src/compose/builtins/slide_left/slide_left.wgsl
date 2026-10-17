// Slide left (builtin slide_left@1): the outgoing frame slides out to the left while the
// incoming frame slides in from the right. Both samples are taken before the select, so
// texture sampling stays in uniform control flow; out-of-frame texels are black.

fn sample_or_black(tex: texture_2d<f32>, uv: vec2<f32>) -> vec4f {
    let inside = uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0;
    let c = textureSample(tex, samp_linear, clamp(uv, vec2(0.0), vec2(1.0)));
    return select(vec4(0.0, 0.0, 0.0, 1.0), c, inside);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let u = clamp(host.progress, 0.0, 1.0);
    let uv = in.uv;
    let sa = sample_or_black(in_from, uv + vec2(u, 0.0));
    let sb = sample_or_black(to, uv + vec2(u - 1.0, 0.0));
    return select(sb, sa, uv.x < (1.0 - u));
}
