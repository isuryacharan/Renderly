// Pack `.cube` LUT filter (builtin lut_filter@1). The LUT is the item's `lut` param, bound as a
// 3D texture; its edge length is read from the texture. Texel-centred UVW, as the former
// compose/effects/pack_lut.wgsl did, so the output is bit-identical.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let c = textureSample(src, samp_linear, in.uv);
    let n = max(f32(textureDimensions(lut).x), 2.0);
    let scale = (n - 1.0) / n;
    let offset = 0.5 / n;
    let uvw = clamp(c.rgb, vec3(0.0), vec3(1.0)) * scale + offset;
    let graded = textureSample(lut, samp_linear, uvw).rgb;
    let rgb = mix(c.rgb, graded, clamp(params.intensity, 0.0, 1.0));
    return vec4f(rgb, c.a);
}
