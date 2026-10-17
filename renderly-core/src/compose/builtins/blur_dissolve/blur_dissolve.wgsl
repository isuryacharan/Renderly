// Blur dissolve (builtin blur_dissolve@1): per-texel noise from an integer hash of the quantised
// uv (exact on every GPU) decides which frame shows, with a soft edge that sweeps with progress.

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let u = clamp(host.progress, 0.0, 1.0);
    let a = textureSample(in_from, samp_linear, in.uv);
    let b = textureSample(to, samp_linear, in.uv);
    let cell = vec2<u32>(floor(in.uv * 4096.0));
    let noise = f32(pcg(cell.x ^ pcg(cell.y)) >> 8u) / 16777216.0;
    let edge = smoothstep(u - 0.15, u + 0.15, noise);
    return mix(b, a, edge);
}
