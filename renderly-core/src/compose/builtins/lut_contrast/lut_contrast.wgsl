// Embedded contrast grade (builtin lut_contrast@1): a smoothstep S-curve with a mild midtone
// push, mixed in by intensity. Matches the former compose/effects/lut.wgsl mode 0.

fn grade_contrast(rgb: vec3f) -> vec3f {
    let x = clamp(rgb, vec3(0.0), vec3(1.0));
    let s = x * x * (3.0 - 2.0 * x);
    return clamp((s - vec3(0.5)) * 1.25 + vec3(0.5), vec3(0.0), vec3(1.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let c = textureSample(src, samp_linear, in.uv);
    let graded = grade_contrast(c.rgb);
    let rgb = mix(c.rgb, graded, clamp(params.intensity, 0.0, 1.0));
    return vec4f(rgb, c.a);
}
