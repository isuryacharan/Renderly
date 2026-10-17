// Crossfade (crossfade@1): mix of the outgoing and incoming frames by the host's progress.
// Matches the former transition.wgsl kind 0 operation for operation.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let u = clamp(host.progress, 0.0, 1.0);
    // `from` is a WGSL reserved word, so the runtime binds that input as `in_from`.
    let a = textureSample(in_from, samp_linear, in.uv);
    let b = textureSample(to, samp_linear, in.uv);
    return mix(a, b, u);
}
