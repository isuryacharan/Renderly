// Embedded warm grade (builtin lut_warm@1): a fixed RGB matrix (lift red, pull blue), mixed in
// by intensity. Matches the former compose/effects/lut.wgsl mode 1.

fn grade_warm(rgb: vec3f) -> vec3f {
    let m = mat3x3<f32>(
        vec3<f32>(1.08, 0.04, 0.0),
        vec3<f32>(0.02, 1.0, 0.0),
        vec3<f32>(-0.06, -0.02, 0.90),
    );
    return clamp(m * rgb, vec3(0.0), vec3(1.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let c = textureSample(src, samp_linear, in.uv);
    let graded = grade_warm(c.rgb);
    let rgb = mix(c.rgb, graded, clamp(params.intensity, 0.0, 1.0));
    return vec4f(rgb, c.a);
}
