// Exposure (stops), contrast around mid grey and saturation (builtin color_adjust@1). The
// operations match the former compose/effects/color_adjust.wgsl step for step.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let c = textureSample(src, samp_linear, in.uv);
    var rgb = c.rgb * pow(2.0, params.exposure);
    rgb = (rgb - vec3(0.5)) * params.contrast + vec3(0.5);
    let luma = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
    rgb = mix(vec3(luma), rgb, params.saturation);
    return vec4f(clamp(rgb, vec3(0.0), vec3(1.0)), c.a);
}
