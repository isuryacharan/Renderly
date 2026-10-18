// Tint v2: desaturates by `amount` (a different picture from v1, so it is a new version).

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let c = textureSample(src, samp_linear, in.uv);
    let luma = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    return vec4f(mix(c.rgb, vec3(luma), clamp(params.amount, 0.0, 1.0)), c.a);
}
