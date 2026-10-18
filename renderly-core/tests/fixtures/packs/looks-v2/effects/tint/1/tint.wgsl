// Tint v1: pulls green and blue down by `amount`.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let c = textureSample(src, samp_linear, in.uv);
    let tinted = c.rgb * vec3(1.0, 0.6, 0.6);
    return vec4f(mix(c.rgb, tinted, clamp(params.amount, 0.0, 1.0)), c.a);
}
