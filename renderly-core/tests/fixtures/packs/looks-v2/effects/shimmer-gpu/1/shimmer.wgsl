// Never loaded: the item requires gpu.compute, which this build does not have.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    return textureSample(src, samp_linear, in.uv);
}
