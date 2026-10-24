// Adjustment-layer mix: out = mix(unadjusted, adjusted, opacity * mask), written back as
// premultiplied alpha (the accumulation target's convention; alpha == 1 at the root, where
// this is a plain mix). Both inputs are straight-alpha pictures of the whole canvas.
//
// Mask coverage mirrors mask.wgsl (keep in sync), except `mode == 3u` means "no mask" and
// the UVs are output-frame (canvas) coordinates because an adjustment has no source frame.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct MixParams {
    bounds: vec4<f32>,
    feather: f32,
    invert: f32,
    mode: u32,   // 0=rect, 1=ellipse, 2=raster, 3=none
    opacity: f32,
    blend: u32,  // BlendMode::shader_index; 0 = normal
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

@group(0) @binding(0) var base_tex: texture_2d<f32>;
@group(0) @binding(1) var adjusted_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: MixParams;
@group(0) @binding(4) var matte_tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
    );
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
    );
    var out: VertexOutput;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

fn soft_edge(dist_outside: f32, feather: f32) -> f32 {
    if (dist_outside <= 0.0) { return 1.0; }
    if (feather <= 1e-6) { return 0.0; }
    return clamp(1.0 - dist_outside / feather, 0.0, 1.0);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Sample everything up front (uniform control flow).
    let base = textureSample(base_tex, samp, input.uv);
    let adjusted = textureSample(adjusted_tex, samp, input.uv);
    let matte = textureSample(matte_tex, samp, input.uv).r;

    var mask_a = 1.0;
    if (params.mode == 0u) {
        let x0 = params.bounds.x;
        let y0 = params.bounds.y;
        let x1 = x0 + params.bounds.z;
        let y1 = y0 + params.bounds.w;
        let dx = max(max(x0 - input.uv.x, input.uv.x - x1), 0.0);
        let dy = max(max(y0 - input.uv.y, input.uv.y - y1), 0.0);
        let dist = length(vec2<f32>(dx, dy));
        mask_a = soft_edge(dist, params.feather * 0.15);
    } else if (params.mode == 1u) {
        let rx = max(params.bounds.z, 1e-4);
        let ry = max(params.bounds.w, 1e-4);
        let nx = (input.uv.x - params.bounds.x) / rx;
        let ny = (input.uv.y - params.bounds.y) / ry;
        let d = length(vec2<f32>(nx, ny));
        mask_a = soft_edge(max(d - 1.0, 0.0), max(params.feather, 0.001));
    } else if (params.mode == 2u) {
        var a = matte;
        if (params.feather > 0.0) {
            let t = params.feather * 0.5;
            a = smoothstep(0.5 - t, 0.5 + t, a);
        }
        mask_a = a;
    }
    if (params.mode != 3u && params.invert > 0.5) {
        mask_a = 1.0 - mask_a;
    }

    var adjusted_mixed = adjusted;
    if (params.blend != 0u) {
        // The adjusted picture is the "source" blended over the unadjusted one (both whole
        // canvas); the result is converted back to straight alpha for the mix below.
        let backdrop = vec4<f32>(base.rgb * base.a, base.a);
        let blended = blend_over(params.blend, backdrop, adjusted.rgb, adjusted.a);
        let inv = select(0.0, 1.0 / max(blended.a, 1e-6), blended.a > 0.0);
        adjusted_mixed = vec4<f32>(clamp(blended.rgb * inv, vec3<f32>(0.0), vec3<f32>(1.0)), blended.a);
    }
    let k = clamp(params.opacity, 0.0, 1.0) * mask_a;
    let m = mix(base, adjusted_mixed, k);
    return vec4<f32>(m.rgb * m.a, m.a);
}
