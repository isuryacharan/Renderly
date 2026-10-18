// Deliberately invalid WGSL: the item must fail to load and the rest of the pack must not.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    return vec4f(;
}
