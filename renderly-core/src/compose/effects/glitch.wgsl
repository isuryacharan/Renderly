// Digital glitch: RGB split + horizontal slice offset.

struct GlitchParams {
    intensity: f32,
    slice: f32,
    time_seed: f32,
    _pad: f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: GlitchParams;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    var positions = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    var uvs = array<vec2<f32>, 6>(
        vec2(0.0, 1.0), vec2(1.0, 1.0), vec2(0.0, 0.0),
        vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(1.0, 0.0),
    );
    var out: VsOut;
    out.pos = vec4(positions[idx], 0.0, 1.0);
    out.uv = uvs[idx];
    return out;
}

// Integer PCG hash. The usual `fract(sin(x) * 43758.5)` depends on each GPU's `sin`
// precision, so the same frame glitched differently on different machines (and preview could
// differ from export). Integer ops are exact everywhere.
fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn hash(p: f32) -> f32 {
    let h = pcg(u32(p) ^ pcg(bitcast<u32>(params.time_seed)));
    return f32(h >> 8u) / 16777216.0;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let intensity = clamp(params.intensity, 0.0, 1.0);
    if (intensity < 0.001) {
        return textureSample(src, samp, in.uv);
    }

    let band = floor(in.uv.y * (4.0 + params.slice * 28.0));
    let offset = (hash(band) - 0.5) * 0.08 * intensity;
    let split = 0.012 * intensity;

    let uv_r = clamp(in.uv + vec2(offset + split, 0.0), vec2(0.0), vec2(1.0));
    let uv_g = clamp(in.uv + vec2(offset, 0.0), vec2(0.0), vec2(1.0));
    let uv_b = clamp(in.uv + vec2(offset - split, 0.0), vec2(0.0), vec2(1.0));

    let r = textureSample(src, samp, uv_r).r;
    let g = textureSample(src, samp, uv_g).g;
    let b = textureSample(src, samp, uv_b).b;
    let a = textureSample(src, samp, in.uv).a;
    return vec4(r, g, b, a);
}
