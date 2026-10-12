struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct ChromaParams {
    key_rgb: vec4<f32>,
    tolerance: f32,
    softness: f32,
    spill: f32,
    _pad: f32,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> params: ChromaParams;

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

// Straight (non-premultiplied) alpha throughout: effect textures hold straight RGBA and the
// compositor blends with ALPHA_BLENDING (src.rgb * src.a + dst.rgb * (1 - src.a)), so keying
// only rewrites `a` (multiplying any existing alpha) and despill only rewrites `rgb`.
// Key distance is Euclidean in RGB; alpha ramps linearly from 0 at `tolerance` to 1 at
// `tolerance + softness`.
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let c = textureSample(src_tex, src_sampler, input.uv);
    let dist = length(c.rgb - params.key_rgb.xyz);
    let soft = max(params.softness, 1e-4);
    var alpha = c.a;
    if (dist <= params.tolerance) {
        alpha = 0.0;
    } else if (dist < params.tolerance + soft) {
        alpha = c.a * ((dist - params.tolerance) / soft);
    }

    // Spill suppression: remove the part of the pixel's chroma (RGB minus its mean) that
    // points toward the key colour's chroma direction. 0 = off (exact legacy output).
    var rgb = c.rgb;
    if (params.spill > 0.0) {
        let key = params.key_rgb.xyz;
        let kc = key - vec3<f32>((key.x + key.y + key.z) / 3.0);
        let kl = length(kc);
        if (kl > 1e-4) {
            let dir = kc / kl;
            let pc = rgb - vec3<f32>((rgb.x + rgb.y + rgb.z) / 3.0);
            let along = max(dot(pc, dir), 0.0);
            rgb = clamp(rgb - dir * along * params.spill, vec3<f32>(0.0), vec3<f32>(1.0));
        }
    }
    return vec4<f32>(rgb, alpha);
}
