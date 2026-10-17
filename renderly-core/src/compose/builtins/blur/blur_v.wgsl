// Vertical pass of the builtin separable Gaussian blur (blur@1); see blur_h.wgsl. `texel` is
// (0, 1/height), computed on the CPU by the item's derived value `inv_h`.

fn blur_tap_1d(tex: texture_2d<f32>, uv: vec2f, texel: vec2f, radius: f32) -> vec4f {
    if (radius < 0.5) {
        return textureSample(tex, samp_linear, uv);
    }

    let sigma = max(radius * 0.5, 0.01);
    let taps = i32(clamp(ceil(radius), 1.0, 16.0));
    var sum = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var i = -taps; i <= taps; i = i + 1) {
        let fi = f32(i);
        let w = exp(-0.5 * fi * fi / (sigma * sigma));
        let suv = uv + texel * fi;
        sum = sum + textureSample(tex, samp_linear, suv) * w;
        wsum = wsum + w;
    }
    return sum / wsum;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    return blur_tap_1d(blurred, in.uv, vec2f(0.0, params.inv_h), params.radius);
}
