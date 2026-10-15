// Dual-input transition pass. Samples outgoing (A) and incoming (B) full-frame textures.
// Uniform: progress 0..1, kind id (see TransitionKind::shader_id).

struct TransitionParams {
    progress: f32,
    kind: u32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var tex_a: texture_2d<f32>;
@group(0) @binding(1) var tex_b: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: TransitionParams;

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

// NOTE: textureSample must stay in uniform control flow (WGSL uniformity analysis — the
// browser's WGSL compiler enforces it strictly for the wasm preview compositor, while
// native naga is lenient). Sample unconditionally at a clamped uv, then select() black for
// out-of-range coordinates instead of branching on the varying before the sample.
fn sample_or_black(tex: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let inside = uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0;
    let c = textureSample(tex, samp, clamp(uv, vec2(0.0), vec2(1.0)));
    return select(vec4(0.0, 0.0, 0.0, 1.0), c, inside);
}

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let u = clamp(params.progress, 0.0, 1.0);
    let uv = in.uv;
    let a = textureSample(tex_a, samp, uv);
    let b = textureSample(tex_b, samp, uv);

    // 0 crossfade
    if (params.kind == 0u) {
        return mix(a, b, u);
    }
    // 1 fade_black
    if (params.kind == 1u) {
        if (u < 0.5) {
            return mix(a, vec4(0.0, 0.0, 0.0, 1.0), u * 2.0);
        }
        return mix(vec4(0.0, 0.0, 0.0, 1.0), b, (u - 0.5) * 2.0);
    }
    // 2 wipe_left (B reveals from left)
    if (params.kind == 2u) {
        return select(a, b, uv.x < u);
    }
    // 3 wipe_right
    if (params.kind == 3u) {
        return select(a, b, uv.x > (1.0 - u));
    }
    // 4 wipe_up
    if (params.kind == 4u) {
        return select(a, b, uv.y < u);
    }
    // 5 wipe_down
    if (params.kind == 5u) {
        return select(a, b, uv.y > (1.0 - u));
    }
    // 6 slide_left — both samples hoisted out of the varying-dependent branch (uniformity,
    // see sample_or_black's note); select() picks per pixel.
    if (params.kind == 6u) {
        let sa = sample_or_black(tex_a, uv + vec2(u, 0.0));
        let sb = sample_or_black(tex_b, uv + vec2(u - 1.0, 0.0));
        return select(sb, sa, uv.x < (1.0 - u));
    }
    // 7 slide_right
    if (params.kind == 7u) {
        let sa = sample_or_black(tex_a, uv - vec2(u, 0.0));
        let sb = sample_or_black(tex_b, uv - vec2(u - 1.0, 0.0));
        return select(sb, sa, uv.x > u);
    }
    // 8 iris
    if (params.kind == 8u) {
        let d = distance(uv, vec2(0.5, 0.5));
        let radius = u * 0.75;
        return select(a, b, d < radius);
    }
    // 9 blur_dissolve: per-texel noise from an integer hash of the quantised uv (exact on every
    // GPU, unlike a `fract(sin(..))` hash).
    let cell = vec2<u32>(floor(uv * 4096.0));
    let noise = f32(pcg(cell.x ^ pcg(cell.y)) >> 8u) / 16777216.0;
    let edge = smoothstep(u - 0.15, u + 0.15, noise);
    return mix(b, a, edge);
}
