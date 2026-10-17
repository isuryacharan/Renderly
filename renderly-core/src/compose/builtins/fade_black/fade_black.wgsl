// Fade through black (builtin fade_black@1): the outgoing frame fades to black over the first
// half of the transition, the incoming frame fades up over the second half.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let u = clamp(host.progress, 0.0, 1.0);
    let a = textureSample(in_from, samp_linear, in.uv);
    let b = textureSample(to, samp_linear, in.uv);
    let black = vec4f(0.0, 0.0, 0.0, 1.0);
    if (u < 0.5) {
        return mix(a, black, u * 2.0);
    }
    return mix(black, b, (u - 0.5) * 2.0);
}
