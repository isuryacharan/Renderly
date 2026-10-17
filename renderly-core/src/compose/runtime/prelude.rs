//! Generated WGSL prelude and uniform layout for effect passes.
//!
//! Every pass shader is `prelude(pass) + author code`. The author writes only the fragment
//! entry point; the prelude declares, at module scope:
//!
//! - `struct Host` and `var<uniform> host` (group 0, binding 0): output and input sizes,
//!   time, progress, frame and seed. Fixed layout, [`HOST_SIZE`] bytes.
//! - `struct Params` and `var<uniform> params` (group 0, binding 1): one field per non-LUT
//!   param (declaration order), then one per derived value. Padding fields `_padN` are
//!   inserted so every member sits at its natural WGSL offset; the struct is padded to its
//!   alignment. Built by [`UniformLayout::new`], which the executor also uses to write bytes.
//! - `samp_linear` (group 0, binding 2) and `samp_nearest` (group 0, binding 3): clamp-to-edge.
//! - one `texture_2d<f32>` (or `texture_3d<f32>` for a LUT param) per input of the pass, at
//!   group 1, binding = the input's index in the pass's `inputs`.
//! - for each file texture input, a sampler named `<texture>_sampler` at group 2, binding =
//!   the input's index, honouring the texture's filter and wrap.
//! - `struct VsOut` and a fullscreen-triangle `vs_main`.
//!
//! The generator output is validated with naga by the executor at load time.

use super::manifest::UniformTy;
use super::HostValues;

/// Byte size of the fixed `Host` block.
pub const HOST_SIZE: usize = 40;

/// Shader-side value of one uniform field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UniformValue {
    F32(f32),
    I32(i32),
    Vec2([f32; 2]),
    Vec4([f32; 4]),
}

/// One member of the generated `Params` struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutField {
    pub name: String,
    pub ty: UniformTy,
    pub offset: usize,
    /// Padding inserted by the generator (never written from a value).
    pub padding: bool,
}

/// The byte layout of the `Params` struct, matching WGSL uniform address-space rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniformLayout {
    pub fields: Vec<LayoutField>,
    pub size: usize,
}

fn round_up(v: usize, align: usize) -> usize {
    v.div_ceil(align) * align
}

impl UniformLayout {
    /// Lays out `fields` (name and type, in declaration order) the way WGSL does, inserting
    /// `f32` padding wherever a member's alignment would leave a gap.
    pub fn new(fields: &[(String, UniformTy)]) -> UniformLayout {
        let mut out: Vec<LayoutField> = Vec::with_capacity(fields.len() + 2);
        let mut offset = 0usize;
        let mut align = 4usize;
        let mut pad_index = 0usize;
        for (name, ty) in fields {
            let target = round_up(offset, ty.align());
            while offset < target {
                out.push(LayoutField {
                    name: format!("_pad{pad_index}"),
                    ty: UniformTy::F32,
                    offset,
                    padding: true,
                });
                pad_index += 1;
                offset += 4;
            }
            out.push(LayoutField {
                name: name.clone(),
                ty: *ty,
                offset,
                padding: false,
            });
            offset += ty.size();
            align = align.max(ty.align());
        }
        if out.is_empty() {
            // WGSL structs need at least one member: a vec4 keeps the block 16 bytes.
            out.push(LayoutField {
                name: format!("_pad{pad_index}"),
                ty: UniformTy::Vec4,
                offset: 0,
                padding: true,
            });
            pad_index += 1;
            offset = 16;
            align = 16;
        }
        let size = round_up(offset, align);
        while offset < size {
            out.push(LayoutField {
                name: format!("_pad{pad_index}"),
                ty: UniformTy::F32,
                offset,
                padding: true,
            });
            pad_index += 1;
            offset += 4;
        }
        UniformLayout { fields: out, size }
    }

    /// WGSL source of the `Params` struct for this layout.
    pub fn wgsl_struct(&self) -> String {
        let mut s = String::from("struct Params {\n");
        for f in &self.fields {
            s.push_str(&format!("  {}: {},\n", f.name, f.ty.wgsl()));
        }
        s.push_str("};\n");
        s
    }

    /// Writes uniform values (one per non-padding field, in order) into a byte block of
    /// [`Self::size`] bytes.
    pub fn encode(&self, values: &[UniformValue]) -> Result<Vec<u8>, String> {
        let mut bytes = vec![0u8; self.size];
        let mut values = values.iter();
        for f in self.fields.iter().filter(|f| !f.padding) {
            let v = values
                .next()
                .ok_or_else(|| format!("missing value for uniform '{}'", f.name))?;
            let at = f.offset;
            match (f.ty, v) {
                (UniformTy::F32, UniformValue::F32(x)) => {
                    bytes[at..at + 4].copy_from_slice(&x.to_le_bytes());
                }
                (UniformTy::I32, UniformValue::I32(x)) => {
                    bytes[at..at + 4].copy_from_slice(&x.to_le_bytes());
                }
                (UniformTy::Vec2, UniformValue::Vec2(x)) => {
                    for (i, c) in x.iter().enumerate() {
                        bytes[at + 4 * i..at + 4 * i + 4].copy_from_slice(&c.to_le_bytes());
                    }
                }
                (UniformTy::Vec4, UniformValue::Vec4(x)) => {
                    for (i, c) in x.iter().enumerate() {
                        bytes[at + 4 * i..at + 4 * i + 4].copy_from_slice(&c.to_le_bytes());
                    }
                }
                _ => return Err(format!("value for uniform '{}' has the wrong type", f.name)),
            }
        }
        if values.next().is_some() {
            return Err("more uniform values than fields".to_string());
        }
        Ok(bytes)
    }
}

/// The identifier a pass input is bound under in WGSL. Most names are used as they are; a name
/// that is a WGSL reserved word (`from`, `filter`, `target`, ...) gets an `in_` prefix, so the
/// author's shader refers to `in_from`.
pub fn wgsl_input_name(name: &str) -> String {
    if super::manifest::WGSL_RESERVED.contains(&name) {
        format!("in_{name}")
    } else {
        name.to_string()
    }
}

/// WGSL source of the fixed `Host` struct.
pub fn host_wgsl() -> &'static str {
    "struct Host {\n\
     \x20 resolution: vec2f,\n\
     \x20 src_size: vec2f,\n\
     \x20 time: f32,\n\
     \x20 duration: f32,\n\
     \x20 progress: f32,\n\
     \x20 frame: u32,\n\
     \x20 seed: u32,\n\
     \x20 _pad: u32,\n\
     };\n"
}

/// Encodes the `Host` block for one pass: `resolution` is that pass's output size, `src_size`
/// the effect's input frame size.
pub fn encode_host(host: &HostValues, resolution: (u32, u32), src_size: (u32, u32)) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HOST_SIZE);
    let words: [[u8; 4]; 10] = [
        (resolution.0 as f32).to_le_bytes(),
        (resolution.1 as f32).to_le_bytes(),
        (src_size.0 as f32).to_le_bytes(),
        (src_size.1 as f32).to_le_bytes(),
        (host.time as f32).to_le_bytes(),
        (host.duration as f32).to_le_bytes(),
        (host.progress as f32).to_le_bytes(),
        host.frame.to_le_bytes(),
        host.seed.to_le_bytes(),
        0u32.to_le_bytes(),
    ];
    for w in words {
        bytes.extend_from_slice(&w);
    }
    bytes
}

/// One texture or sampler input of a pass, as the prelude declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreludeInput {
    pub name: String,
    /// `true` for a LUT param (`texture_3d`).
    pub lut: bool,
    /// Sampler name when this input is a file texture (`<texture>_sampler`).
    pub sampler: Option<String>,
}

/// The full prelude for one pass.
pub fn pass_prelude(layout: &UniformLayout, inputs: &[PreludeInput]) -> String {
    let mut s = String::from("// ---- generated prelude (not written by authors) ----\n");
    s.push_str(host_wgsl());
    s.push_str(&layout.wgsl_struct());
    s.push_str("@group(0) @binding(0) var<uniform> host: Host;\n");
    s.push_str("@group(0) @binding(1) var<uniform> params: Params;\n");
    s.push_str("@group(0) @binding(2) var samp_linear: sampler;\n");
    s.push_str("@group(0) @binding(3) var samp_nearest: sampler;\n");
    for (i, input) in inputs.iter().enumerate() {
        let ty = if input.lut {
            "texture_3d<f32>"
        } else {
            "texture_2d<f32>"
        };
        s.push_str(&format!(
            "@group(1) @binding({i}) var {}: {ty};\n",
            input.name
        ));
    }
    for (i, input) in inputs.iter().enumerate() {
        if let Some(sampler) = &input.sampler {
            s.push_str(&format!(
                "@group(2) @binding({i}) var {sampler}: sampler;\n"
            ));
        }
    }
    s.push_str(VERTEX);
    s.push_str("// ---- end prelude ----\n");
    s
}

const VERTEX: &str = "struct VsOut {
  @builtin(position) pos: vec4f,
  @location(0) uv: vec2f,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
  var positions = array<vec2f, 6>(
    vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(-1.0, 1.0),
    vec2f(-1.0, 1.0), vec2f(1.0, -1.0), vec2f(1.0, 1.0),
  );
  var uvs = array<vec2f, 6>(
    vec2f(0.0, 1.0), vec2f(1.0, 1.0), vec2f(0.0, 0.0),
    vec2f(0.0, 0.0), vec2f(1.0, 1.0), vec2f(1.0, 0.0),
  );
  var out: VsOut;
  out.pos = vec4f(positions[idx], 0.0, 1.0);
  out.uv = uvs[idx];
  return out;
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_inserts_padding_between_scalars_and_vectors() {
        let l = UniformLayout::new(&[
            ("a".into(), UniformTy::F32),
            ("v".into(), UniformTy::Vec4),
            ("b".into(), UniformTy::I32),
            ("p".into(), UniformTy::Vec2),
        ]);
        let named: Vec<(&str, usize, bool)> = l
            .fields
            .iter()
            .map(|f| (f.name.as_str(), f.offset, f.padding))
            .collect();
        assert_eq!(
            named,
            vec![
                ("a", 0, false),
                ("_pad0", 4, true),
                ("_pad1", 8, true),
                ("_pad2", 12, true),
                ("v", 16, false),
                ("b", 32, false),
                ("_pad3", 36, true),
                ("p", 40, false),
            ]
        );
        assert_eq!(l.size, 48);
    }

    #[test]
    fn empty_params_still_form_a_valid_struct() {
        let l = UniformLayout::new(&[]);
        assert_eq!(l.size, 16);
        assert!(l.wgsl_struct().contains("_pad0: vec4f"));
    }

    #[test]
    fn encode_writes_values_at_their_offsets() {
        let l = UniformLayout::new(&[("a".into(), UniformTy::F32), ("v".into(), UniformTy::Vec4)]);
        let bytes = l
            .encode(&[
                UniformValue::F32(2.0),
                UniformValue::Vec4([1.0, 0.0, 0.5, 1.0]),
            ])
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bytes.len(), 32);
        assert_eq!(
            f32::from_le_bytes(bytes[0..4].try_into().unwrap_or([0; 4])),
            2.0
        );
        assert_eq!(
            f32::from_le_bytes(bytes[16..20].try_into().unwrap_or([0; 4])),
            1.0
        );
        assert_eq!(
            f32::from_le_bytes(bytes[24..28].try_into().unwrap_or([0; 4])),
            0.5
        );
        assert!(l.encode(&[UniformValue::F32(1.0)]).is_err());
        assert!(l
            .encode(&[UniformValue::I32(1), UniformValue::Vec4([0.0; 4])])
            .is_err());
    }

    #[test]
    fn host_block_is_forty_bytes() {
        let host = HostValues {
            width: 320,
            height: 180,
            time: 0.5,
            duration: 1.0,
            progress: 0.25,
            frame: 3,
            seed: 9,
            speed: 1.0,
        };
        let bytes = encode_host(&host, (320, 180), (320, 180));
        assert_eq!(bytes.len(), HOST_SIZE);
        assert_eq!(
            f32::from_le_bytes(bytes[16..20].try_into().unwrap_or([0; 4])),
            0.5
        );
        assert_eq!(
            u32::from_le_bytes(bytes[28..32].try_into().unwrap_or([0; 4])),
            3
        );
        assert_eq!(
            u32::from_le_bytes(bytes[32..36].try_into().unwrap_or([0; 4])),
            9
        );
    }

    #[test]
    fn prelude_declares_bindings_per_input() {
        let l = UniformLayout::new(&[("radius".into(), UniformTy::F32)]);
        let p = pass_prelude(
            &l,
            &[
                PreludeInput {
                    name: "src".into(),
                    lut: false,
                    sampler: None,
                },
                PreludeInput {
                    name: "noise".into(),
                    lut: false,
                    sampler: Some("noise_sampler".into()),
                },
                PreludeInput {
                    name: "cube".into(),
                    lut: true,
                    sampler: None,
                },
            ],
        );
        assert!(p.contains("@group(1) @binding(0) var src: texture_2d<f32>;"));
        assert!(p.contains("@group(1) @binding(2) var cube: texture_3d<f32>;"));
        assert!(p.contains("@group(2) @binding(1) var noise_sampler: sampler;"));
        assert!(p.contains("fn vs_main"));
    }
}
