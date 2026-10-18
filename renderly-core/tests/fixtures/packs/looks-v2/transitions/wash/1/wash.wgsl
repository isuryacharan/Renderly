// Wash: a crossfade whose progress is eased in (u * u).

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let u = clamp(host.progress, 0.0, 1.0);
    // `from` is a WGSL reserved word, so the runtime binds that input as `in_from`.
    let a = textureSample(in_from, samp_linear, in.uv);
    let b = textureSample(to, samp_linear, in.uv);
    return mix(a, b, u * u);
}
