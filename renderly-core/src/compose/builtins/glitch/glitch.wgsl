// Digital glitch (builtin glitch@1): RGB split and per-band horizontal offset. The integer PCG
// hash is exact on every GPU (the usual fract(sin(x)) hash differs by GPU). Operations match the
// former compose/effects/glitch.wgsl. `seed` is the stored seed param, bit-cast for the hash.

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn hash(p: f32) -> f32 {
    let h = pcg(u32(p) ^ pcg(bitcast<u32>(params.seed)));
    return f32(h >> 8u) / 16777216.0;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let intensity = clamp(params.intensity, 0.0, 1.0);

    let band = floor(in.uv.y * (4.0 + params.slice * 28.0));
    let offset = (hash(band) - 0.5) * 0.08 * intensity;
    let split = 0.012 * intensity;

    let uv_r = clamp(in.uv + vec2(offset + split, 0.0), vec2(0.0), vec2(1.0));
    let uv_g = clamp(in.uv + vec2(offset, 0.0), vec2(0.0), vec2(1.0));
    let uv_b = clamp(in.uv + vec2(offset - split, 0.0), vec2(0.0), vec2(1.0));

    let r = textureSample(src, samp_linear, uv_r).r;
    let g = textureSample(src, samp_linear, uv_g).g;
    let b = textureSample(src, samp_linear, uv_b).b;
    let a = textureSample(src, samp_linear, in.uv).a;
    return vec4f(r, g, b, a);
}
