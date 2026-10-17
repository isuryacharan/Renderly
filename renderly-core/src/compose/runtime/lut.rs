//! 3D LUT textures for `lut` params and pass inputs.
//!
//! A `.cube` LUT (already parsed by [`crate::packs::parse_cube_text`]) is uploaded as a
//! `texture_3d` in `Rgba8Unorm`, with the same byte encoding the existing CPU/GPU pack LUT
//! path uses ([`crate::packs::cube_lut_upload_bytes`]), so both paths see identical values.
//! Passes sample it with a linear sampler; the design's texel-centred UVW convention is the
//! shader's job (see `builtins` for the pack-LUT filter). PNG LUT layouts (grid, strip, Hald)
//! are not read by this module yet.

use crate::packs::{cube_lut_upload_bytes, CubeLut};

use super::EffectError;

/// Largest `LUT_3D_SIZE` the design allows.
pub const MAX_LUT_SIZE: usize = 65;

/// A LUT uploaded to the GPU. Keep it alive while any effect reads its view.
pub struct Lut3d {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: u32,
}

impl Lut3d {
    /// The `texture_3d` view to bind as a pass input.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Edge length of the cube in texels.
    pub fn size(&self) -> u32 {
        self.size
    }
}

/// Uploads `cube` as a 3D texture. Fails when the size is outside `2..=65` or the data does
/// not hold `size^3` entries.
pub fn upload_cube(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cube: &CubeLut,
) -> Result<Lut3d, EffectError> {
    if cube.size < 2 || cube.size > MAX_LUT_SIZE {
        return Err(EffectError::Limit(format!(
            "LUT_3D_SIZE {} is outside 2..={MAX_LUT_SIZE}",
            cube.size
        )));
    }
    if cube.data.len() != cube.size * cube.size * cube.size {
        return Err(EffectError::Lut(format!(
            "expected {} entries for size {}, found {}",
            cube.size * cube.size * cube.size,
            cube.size,
            cube.data.len()
        )));
    }
    let edge = cube.size as u32;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("effect-lut3d"),
        size: wgpu::Extent3d {
            width: edge,
            height: edge,
            depth_or_array_layers: edge,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let (bytes, bytes_per_row) = pad_rows(
        &cube_lut_upload_bytes(cube),
        edge as usize * 4,
        (edge as usize) * (edge as usize),
    );
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(edge),
        },
        wgpu::Extent3d {
            width: edge,
            height: edge,
            depth_or_array_layers: edge,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D3),
        ..Default::default()
    });
    Ok(Lut3d {
        _texture: texture,
        view,
        size: edge,
    })
}

/// Lays `packed` (`rows` rows of `row_bytes` each, tightly packed) out with rows padded to the
/// 256-byte copy alignment. Returns the padded bytes and the padded row pitch.
pub(crate) fn pad_rows(packed: &[u8], row_bytes: usize, rows: usize) -> (Vec<u8>, u32) {
    let pitch = row_bytes.div_ceil(256) * 256;
    let mut out = vec![0u8; pitch * rows];
    for (r, chunk) in packed.chunks_exact(row_bytes).take(rows).enumerate() {
        out[r * pitch..r * pitch + row_bytes].copy_from_slice(chunk);
    }
    (out, pitch as u32)
}
