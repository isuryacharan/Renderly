// Iris (builtin iris@1): the incoming frame opens as a circle from the centre.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let u = clamp(host.progress, 0.0, 1.0);
    let a = textureSample(in_from, samp_linear, in.uv);
    let b = textureSample(to, samp_linear, in.uv);
    let d = distance(in.uv, vec2(0.5, 0.5));
    let radius = u * 0.75;
    return select(a, b, d < radius);
}
