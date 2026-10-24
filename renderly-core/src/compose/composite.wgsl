struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Cover UV remap + user spatial transform + opacity (Phase 3.1).
struct LayerParams {
    cover_scale: vec2<f32>,
    cover_offset: vec2<f32>,
    user_translate: vec2<f32>,
    user_scale: vec2<f32>,
    rotation_rad: f32,
    opacity: f32,
    blend_mode: u32,
    _pad: u32,
};

@group(0) @binding(2) var<uniform> layer: LayerParams;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Two triangles covering NDC [-1,1]^2 (proper quad — needed for rotation/scale).
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

    var pos = positions[vertex_index] * layer.user_scale;
    let c = cos(layer.rotation_rad);
    let s = sin(layer.rotation_rad);
    pos = vec2<f32>(pos.x * c - pos.y * s, pos.x * s + pos.y * c);
    pos = pos + layer.user_translate;

    var out: VertexOutput;
    out.position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uvs[vertex_index] * layer.cover_scale + layer.cover_offset;
    return out;
}

@group(0) @binding(0) var layer_tex: texture_2d<f32>;
@group(0) @binding(1) var layer_sampler: sampler;

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(layer_tex, layer_sampler, input.uv);
    return vec4<f32>(color.rgb, color.a * layer.opacity);
}

// Blend modes other than normal (see blend.wgsl). The pipeline has blending off: this reads the
// premultiplied accumulation so far (a copy of the target) and writes the whole result for the
// pixels the layer covers. Where the layer is transparent the backdrop passes through.
@group(0) @binding(3) var backdrop_tex: texture_2d<f32>;

@fragment
fn fs_blend(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(layer_tex, layer_sampler, input.uv);
    let backdrop = textureLoad(backdrop_tex, vec2<i32>(input.position.xy), 0);
    return blend_over(layer.blend_mode, backdrop, color.rgb, color.a * layer.opacity);
}
