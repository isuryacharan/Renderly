// Wipe from left (builtin wipe_left@1): the incoming frame reveals from the left edge.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let u = clamp(host.progress, 0.0, 1.0);
    let a = textureSample(in_from, samp_linear, in.uv);
    let b = textureSample(to, samp_linear, in.uv);
    return select(a, b, in.uv.x < u);
}
