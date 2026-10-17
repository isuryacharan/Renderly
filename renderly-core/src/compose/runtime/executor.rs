//! Load-time compilation of an effect item, and its multi-pass wgpu execution.
//!
//! [`CompiledEffect::load`] takes the item's `effect.json` text, its WGSL files and its decoded
//! textures as plain data. It validates the manifest, builds each pass's prelude, joins it with
//! the author's code and checks the result with naga (the same front end and validator wgpu
//! uses), so a broken item fails here instead of on its first frame.
//!
//! [`EffectExecutor`] runs a compiled item on a caller's device. Pipelines, file textures and
//! pooled render targets are cached in the executor. Each run is a pure function of its inputs
//! (params, host values and pixels): no state carries from one frame to the next, so seeking
//! to frame N gives the same result as playing up to it.

use std::collections::{BTreeMap, HashMap};

use super::manifest::{
    EffectManifest, FilterMode, ItemKind, TargetFormat, WrapMode, MAX_UNIFORM_BYTES,
};
use super::prelude::{self, PreludeInput, UniformLayout, HOST_SIZE};
use super::{EffectError, HostValues, ParamValue};

/// Largest file texture side the design allows.
const MAX_TEXTURE_DIM: u32 = 4096;
/// Free render targets kept per size and format before the surplus is dropped.
const POOL_MAX_PER_KEY: usize = 8;
/// Usage of every target and output: rendered into, sampled by later passes, and readable by
/// the caller (`COPY_SRC`, as the compositor's output texture is).
const TARGET_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC);

/// An 8-bit RGBA image decoded by the host (for example from a PNG).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA8, row-major, top row first.
    pub rgba: Vec<u8>,
}

/// Where a pass writes: an intermediate target (by index) or the item's final output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Target(usize),
    Out,
}

/// What a pass input names, resolved at load.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InputRef {
    /// `src`, `from`, `to` or a LUT param key: bound by the caller in [`EffectInputs`].
    External(String),
    /// `prev` or a target: written by an earlier pass.
    Slot(Slot),
    /// A file texture (index into the manifest's `textures`).
    File(usize),
}

struct CompiledPass {
    /// Generated prelude followed by the author's WGSL.
    wgsl: String,
    entry: String,
    inputs: Vec<InputRef>,
    prelude_inputs: Vec<PreludeInput>,
    output: Slot,
    output_format: wgpu::TextureFormat,
}

/// A validated, device-independent effect item.
pub struct CompiledEffect {
    manifest: EffectManifest,
    hash: u64,
    layout: UniformLayout,
    passes: Vec<CompiledPass>,
    textures: Vec<DecodedImage>,
}

impl std::fmt::Debug for CompiledEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledEffect")
            .field("id", &self.manifest.id)
            .field("version", &self.manifest.version)
            .field("passes", &self.passes.len())
            .finish_non_exhaustive()
    }
}

/// 64-bit FNV-1a, used as the item's content key inside one process. Not a store hash.
fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn output_format(format: TargetFormat) -> wgpu::TextureFormat {
    match format {
        TargetFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
        TargetFormat::Rgba16f => wgpu::TextureFormat::Rgba16Float,
    }
}

/// Checks `source` with naga and that `entry` is a fragment entry point.
fn validate_wgsl(source: &str, entry: &str) -> Result<(), String> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(&module)
    .map_err(|e| e.emit_to_string(source))?;
    if module
        .entry_points
        .iter()
        .any(|ep| ep.name == entry && ep.stage == naga::ShaderStage::Fragment)
    {
        Ok(())
    } else {
        Err(format!("no fragment entry point named '{entry}'"))
    }
}

impl CompiledEffect {
    /// Compiles an item. `shaders` maps each pass's `shader` path to its WGSL source;
    /// `textures` maps each file texture's `path` to its decoded pixels.
    pub fn load(
        manifest_json: &str,
        shaders: &BTreeMap<String, String>,
        textures: &BTreeMap<String, DecodedImage>,
    ) -> Result<CompiledEffect, EffectError> {
        let manifest = EffectManifest::parse(manifest_json)?;

        let mut decoded = Vec::with_capacity(manifest.textures.len());
        for t in &manifest.textures {
            let img = textures
                .get(&t.path)
                .ok_or_else(|| EffectError::MissingFile(t.path.clone()))?;
            let bad = |message: String| EffectError::Texture {
                name: t.name.clone(),
                message,
            };
            if img.width == 0
                || img.height == 0
                || img.width > MAX_TEXTURE_DIM
                || img.height > MAX_TEXTURE_DIM
            {
                return Err(bad(format!(
                    "size {}x{} is outside 1..={MAX_TEXTURE_DIM}",
                    img.width, img.height
                )));
            }
            if img.rgba.len() != (img.width as usize) * (img.height as usize) * 4 {
                return Err(bad("pixel data does not match its size".into()));
            }
            decoded.push(img.clone());
        }

        let fields = manifest.uniform_fields();
        let layout = UniformLayout::new(&fields);
        if layout.size + HOST_SIZE > MAX_UNIFORM_BYTES {
            return Err(EffectError::Limit(format!(
                "uniform block is {} bytes (limit {MAX_UNIFORM_BYTES})",
                layout.size + HOST_SIZE
            )));
        }

        let lut_keys: Vec<&str> = manifest.lut_params().map(|p| p.key.as_str()).collect();
        let target_index: BTreeMap<&str, usize> = manifest
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.as_str(), i))
            .collect();
        let file_index: BTreeMap<&str, usize> = manifest
            .textures
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.as_str(), i))
            .collect();

        let mut passes = Vec::with_capacity(manifest.passes.len());
        let mut previous: Option<Slot> = None;
        let mut hash = FNV_OFFSET;
        hash = fnv1a(hash, manifest_json.as_bytes());
        for (name, src) in shaders {
            hash = fnv1a(hash, name.as_bytes());
            hash = fnv1a(hash, src.as_bytes());
        }
        for (name, img) in textures {
            hash = fnv1a(hash, name.as_bytes());
            hash = fnv1a(hash, &img.rgba);
        }

        for (i, pass) in manifest.passes.iter().enumerate() {
            let author = shaders
                .get(&pass.shader)
                .ok_or_else(|| EffectError::MissingFile(pass.shader.clone()))?;
            let mut inputs = Vec::with_capacity(pass.inputs.len());
            let mut prelude_inputs = Vec::with_capacity(pass.inputs.len());
            for name in &pass.inputs {
                let r = match name.as_str() {
                    "src" | "from" | "to" => InputRef::External(name.clone()),
                    "prev" => InputRef::Slot(previous.ok_or_else(|| EffectError::Shader {
                        pass: i,
                        message: "'prev' used in the first pass".into(),
                    })?),
                    other if lut_keys.contains(&other) => InputRef::External(other.to_string()),
                    other if file_index.contains_key(other) => InputRef::File(file_index[other]),
                    other if target_index.contains_key(other) => {
                        InputRef::Slot(Slot::Target(target_index[other]))
                    }
                    other => {
                        return Err(EffectError::Shader {
                            pass: i,
                            message: format!("input '{other}' does not resolve"),
                        })
                    }
                };
                let sampler = match &r {
                    InputRef::File(idx) => Some(format!(
                        "{}_sampler",
                        prelude::wgsl_input_name(&manifest.textures[*idx].name)
                    )),
                    _ => None,
                };
                prelude_inputs.push(PreludeInput {
                    name: prelude::wgsl_input_name(name),
                    lut: lut_keys.contains(&name.as_str()),
                    sampler,
                });
                inputs.push(r);
            }
            let output = if pass.output == "out" {
                Slot::Out
            } else {
                Slot::Target(target_index[pass.output.as_str()])
            };
            let output_format = match output {
                Slot::Out => wgpu::TextureFormat::Rgba8Unorm,
                Slot::Target(t) => output_format(manifest.targets[t].format),
            };
            let prelude = prelude::pass_prelude(&layout, &prelude_inputs);
            let wgsl = format!("{prelude}\n{author}");
            validate_wgsl(&wgsl, &pass.entry)
                .map_err(|message| EffectError::Shader { pass: i, message })?;
            passes.push(CompiledPass {
                wgsl,
                entry: pass.entry.clone(),
                inputs,
                prelude_inputs,
                output,
                output_format,
            });
            previous = Some(output);
        }

        Ok(CompiledEffect {
            manifest,
            hash,
            layout,
            passes,
            textures: decoded,
        })
    }

    pub fn manifest(&self) -> &EffectManifest {
        &self.manifest
    }

    /// True when the item's `bypass` holds for the stored params: the item is the identity and
    /// the caller runs no passes.
    pub fn is_bypassed(
        &self,
        params: &dyn Fn(&str) -> Option<ParamValue>,
    ) -> Result<bool, EffectError> {
        let controls = self
            .manifest
            .resolve_params(params)
            .map_err(EffectError::Param)?;
        Ok(self.manifest.is_bypassed(&controls)?)
    }

    /// Content key of this item (manifest, shaders and textures); changes whenever any of
    /// them does.
    pub fn content_hash(&self) -> u64 {
        self.hash
    }

    /// The full WGSL of pass `index` (prelude plus author code), for inspection.
    pub fn pass_wgsl(&self, index: usize) -> Option<&str> {
        self.passes.get(index).map(|p| p.wgsl.as_str())
    }
}

/// The external inputs of one run, by name: `src` for effects and filters, `from` and `to` for
/// transitions, and one entry per LUT param (named by its key).
#[derive(Default)]
pub struct EffectInputs<'a> {
    views: BTreeMap<String, &'a wgpu::TextureView>,
}

impl<'a> EffectInputs<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(&mut self, name: &str, view: &'a wgpu::TextureView) -> &mut Self {
        self.views.insert(name.to_string(), view);
        self
    }
}

/// The final output of a run. Hand it back with [`EffectExecutor::recycle`] when done.
pub struct EffectOutput {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl EffectOutput {
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

struct PassPipeline {
    pipeline: wgpu::RenderPipeline,
    group1: wgpu::BindGroupLayout,
    group2: wgpu::BindGroupLayout,
    /// Group 2 bind group when the pass has no file textures (it is empty, so created once).
    group2_empty: Option<wgpu::BindGroup>,
}

struct FileTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

/// Byte stride of each pass's `Host` block in the uniform ring: the dynamic-offset alignment.
const UNIFORM_ALIGN: u64 = 256;
/// Bytes reserved for one run's `Params` block (the largest the design allows).
const PARAMS_SLOT: u64 = MAX_UNIFORM_BYTES as u64;
/// Smallest uniform ring allocation.
const RING_MIN_BYTES: u64 = 64 * 1024;

/// Per-frame uniform storage. Every run stages its `Params` and each pass's `Host` block at
/// fresh offsets, and binds them with dynamic offsets, so a frame creates no uniform buffers
/// and no per-run bind groups. The compositor calls [`EffectExecutor::begin_frame`] once per
/// submitted frame, which rewinds the cursor: the queue applies those writes after the previous
/// submission has run, so reusing the offsets is safe.
struct UniformRing {
    buffer: wgpu::Buffer,
    group0: wgpu::BindGroup,
    capacity: u64,
    cursor: u64,
}

/// Where one run's uniforms sit in the ring.
struct StagedUniforms {
    params_offset: u32,
    host_offsets: Vec<u32>,
}

/// Runs compiled effects on one device. Create one per device and keep it.
pub struct EffectExecutor {
    sampler_linear: wgpu::Sampler,
    sampler_nearest: wgpu::Sampler,
    group0_layout: wgpu::BindGroupLayout,
    ring: Option<UniformRing>,
    pipelines: HashMap<(u64, usize, bool), PassPipeline>,
    files: HashMap<(u64, usize), FileTexture>,
    pool: HashMap<(u32, u32, wgpu::TextureFormat), Vec<wgpu::Texture>>,
}

/// Where a run's final pass writes.
pub enum FinalTarget<'a> {
    /// A pooled output texture, returned as [`EffectOutput`].
    Output,
    /// Straight into `view`, alpha-blended over its contents (`ALPHA_BLENDING`, no clear), so
    /// a transition lands on the layer it replaces without an extra copy pass.
    Blend(&'a wgpu::TextureView),
}

impl EffectExecutor {
    pub fn new(device: &wgpu::Device) -> Self {
        let sampler_linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("effect-linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let sampler_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("effect-nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let group0_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect-group0"),
            entries: &[
                dynamic_uniform_entry(0, fragment, Some(prelude::HOST_SIZE as u64)),
                dynamic_uniform_entry(1, fragment, None),
                sampler_entry(2, fragment),
                sampler_entry(3, fragment),
            ],
        });
        Self {
            sampler_linear,
            sampler_nearest,
            group0_layout,
            ring: None,
            pipelines: HashMap::new(),
            files: HashMap::new(),
            pool: HashMap::new(),
        }
    }

    /// Returns a finished output to the pool.
    pub fn recycle(&mut self, out: EffectOutput) {
        self.put(out.texture);
    }

    /// Marks the start of a submitted frame: uniform staging restarts at the ring's beginning.
    /// Call it once before each frame's effect runs, at the point the previous frame has been
    /// submitted.
    pub fn begin_frame(&mut self) {
        if let Some(ring) = self.ring.as_mut() {
            ring.cursor = 0;
        }
    }

    /// A pooled `width` x `height` output, for passes the caller records itself (for example a
    /// mask). Hand it back with [`Self::recycle`].
    pub fn take_output(&mut self, device: &wgpu::Device, width: u32, height: u32) -> EffectOutput {
        let texture = self.take(device, width, height, wgpu::TextureFormat::Rgba8Unorm);
        let view = texture.create_view(&Default::default());
        EffectOutput {
            texture,
            view,
            width,
            height,
        }
    }

    fn take(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        if let Some(t) = self
            .pool
            .get_mut(&(width, height, format))
            .and_then(Vec::pop)
        {
            return t;
        }
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("effect-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: TARGET_USAGE,
            view_formats: &[],
        })
    }

    fn put(&mut self, texture: wgpu::Texture) {
        let key = (texture.width(), texture.height(), texture.format());
        let slot = self.pool.entry(key).or_default();
        if slot.len() < POOL_MAX_PER_KEY {
            slot.push(texture);
        }
    }

    fn ensure_pipeline(
        &mut self,
        device: &wgpu::Device,
        effect: &CompiledEffect,
        index: usize,
        blend: bool,
    ) {
        let key = (effect.hash, index, blend);
        if self.pipelines.contains_key(&key) {
            return;
        }
        let pass = &effect.passes[index];
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let group1_entries: Vec<wgpu::BindGroupLayoutEntry> = pass
            .prelude_inputs
            .iter()
            .enumerate()
            .map(|(i, input)| wgpu::BindGroupLayoutEntry {
                binding: i as u32,
                visibility: fragment,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: if input.lut {
                        wgpu::TextureViewDimension::D3
                    } else {
                        wgpu::TextureViewDimension::D2
                    },
                    multisampled: false,
                },
                count: None,
            })
            .collect();
        let group1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect-group1"),
            entries: &group1_entries,
        });
        let group2_entries: Vec<wgpu::BindGroupLayoutEntry> = pass
            .prelude_inputs
            .iter()
            .enumerate()
            .filter(|(_, input)| input.sampler.is_some())
            .map(|(i, _)| sampler_entry(i as u32, fragment))
            .collect();
        let group2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect-group2"),
            entries: &group2_entries,
        });
        let group2_empty = group2_entries.is_empty().then(|| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect-group2-empty"),
                layout: &group2,
                entries: &[],
            })
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effect-pipeline-layout"),
            bind_group_layouts: &[Some(&self.group0_layout), Some(&group1), Some(&group2)],
            immediate_size: 0,
        });
        let blend_state = blend.then_some(wgpu::BlendState::ALPHA_BLENDING);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("effect-pass"),
            source: wgpu::ShaderSource::Wgsl(pass.wgsl.as_str().into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("effect-pass"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(pass.entry.as_str()),
                targets: &[Some(wgpu::ColorTargetState {
                    format: pass.output_format,
                    blend: blend_state,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.insert(
            key,
            PassPipeline {
                pipeline,
                group1,
                group2,
                group2_empty,
            },
        );
    }

    /// Stages one run's `Params` and each pass's `Host` block in the uniform ring.
    fn stage_uniforms(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &[u8],
        hosts: &[Vec<u8>],
    ) -> StagedUniforms {
        let need = PARAMS_SLOT + hosts.len() as u64 * UNIFORM_ALIGN;
        let fits = self
            .ring
            .as_ref()
            .is_some_and(|r| r.cursor + need <= r.capacity);
        if !fits {
            let capacity = (self
                .ring
                .as_ref()
                .map_or(RING_MIN_BYTES, |r| r.capacity * 2))
            .max(need.next_power_of_two());
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("effect-uniform-ring"),
                size: capacity,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect-group0"),
                layout: &self.group0_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &buffer,
                            offset: 0,
                            size: wgpu::BufferSize::new(prelude::HOST_SIZE as u64),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &buffer,
                            offset: 0,
                            size: wgpu::BufferSize::new(PARAMS_SLOT),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler_linear),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler_nearest),
                    },
                ],
            });
            self.ring = Some(UniformRing {
                buffer,
                group0,
                capacity,
                cursor: 0,
            });
        }
        let Some(ring) = self.ring.as_mut() else {
            return StagedUniforms {
                params_offset: 0,
                host_offsets: Vec::new(),
            };
        };
        let base = ring.cursor;
        ring.cursor += need;
        let write = |offset: u64, bytes: &[u8]| {
            // write_buffer needs 4-byte multiples; pad with zeros.
            let mut padded = bytes.to_vec();
            padded.resize(bytes.len().div_ceil(4) * 4, 0);
            queue.write_buffer(&ring.buffer, offset, &padded);
        };
        write(base, params);
        let mut host_offsets = Vec::with_capacity(hosts.len());
        for (i, host) in hosts.iter().enumerate() {
            let offset = base + PARAMS_SLOT + i as u64 * UNIFORM_ALIGN;
            write(offset, host);
            host_offsets.push(offset as u32);
        }
        StagedUniforms {
            params_offset: base as u32,
            host_offsets,
        }
    }

    fn ensure_file(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        effect: &CompiledEffect,
        index: usize,
    ) {
        let key = (effect.hash, index);
        if self.files.contains_key(&key) {
            return;
        }
        let spec = &effect.manifest.textures[index];
        let img = &effect.textures[index];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("effect-file-texture"),
            size: wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (bytes, row_bytes) =
            super::lut::pad_rows(&img.rgba, img.width as usize * 4, img.height as usize);
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
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(img.height),
            },
            wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
        );
        let filter = match spec.filter {
            FilterMode::Linear => wgpu::FilterMode::Linear,
            FilterMode::Nearest => wgpu::FilterMode::Nearest,
        };
        let address = match spec.wrap {
            WrapMode::Clamp => wgpu::AddressMode::ClampToEdge,
            WrapMode::Repeat => wgpu::AddressMode::Repeat,
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("effect-file-sampler"),
            mag_filter: filter,
            min_filter: filter,
            address_mode_u: address,
            address_mode_v: address,
            address_mode_w: address,
            ..Default::default()
        });
        let view = texture.create_view(&Default::default());
        self.files.insert(
            key,
            FileTexture {
                _texture: texture,
                view,
                sampler,
            },
        );
    }

    /// Runs `effect` once into a pooled output. `params` looks up the stored control value of a
    /// param by key (`None` takes the default). The effect's input frame is `host.width` x
    /// `host.height`; intermediate targets scale from it. The caller submits `encoder`.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        effect: &CompiledEffect,
        inputs: &EffectInputs<'_>,
        params: &dyn Fn(&str) -> Option<ParamValue>,
        host: &HostValues,
    ) -> Result<EffectOutput, EffectError> {
        self.encode(
            device,
            queue,
            encoder,
            effect,
            inputs,
            params,
            host,
            FinalTarget::Output,
        )?
        .ok_or_else(|| EffectError::Gpu("the run produced no output".into()))
    }

    /// As [`Self::run`], but the final pass is alpha-blended straight into `dest` (see
    /// [`FinalTarget::Blend`]).
    #[allow(clippy::too_many_arguments)]
    pub fn run_blended(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        effect: &CompiledEffect,
        inputs: &EffectInputs<'_>,
        params: &dyn Fn(&str) -> Option<ParamValue>,
        host: &HostValues,
        dest: &wgpu::TextureView,
    ) -> Result<(), EffectError> {
        self.encode(
            device,
            queue,
            encoder,
            effect,
            inputs,
            params,
            host,
            FinalTarget::Blend(dest),
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        effect: &CompiledEffect,
        inputs: &EffectInputs<'_>,
        params: &dyn Fn(&str) -> Option<ParamValue>,
        host: &HostValues,
        final_target: FinalTarget<'_>,
    ) -> Result<Option<EffectOutput>, EffectError> {
        if host.width == 0 || host.height == 0 {
            return Err(EffectError::Limit("effect input has no pixels".into()));
        }
        let manifest = &effect.manifest;
        let controls = manifest
            .resolve_params(params)
            .map_err(EffectError::Param)?;

        let required: Vec<&str> = match manifest.kind {
            ItemKind::Effect | ItemKind::Filter => vec!["src"],
            ItemKind::Transition => vec!["from", "to"],
        }
        .into_iter()
        .chain(manifest.lut_params().map(|p| p.key.as_str()))
        .collect();
        for name in required {
            if !inputs.views.contains_key(name) {
                return Err(EffectError::MissingInput(name.to_string()));
            }
        }

        let values = manifest.evaluate_uniforms(&controls, host)?;
        let params_bytes = effect.layout.encode(&values).map_err(EffectError::Param)?;

        let last = effect.passes.len().saturating_sub(1);
        let blended = matches!(final_target, FinalTarget::Blend(_));
        for index in 0..effect.passes.len() {
            self.ensure_pipeline(device, effect, index, blended && index == last);
        }
        for index in 0..manifest.textures.len() {
            self.ensure_file(device, queue, effect, index);
        }

        let (width, height) = (host.width, host.height);
        let host_blocks: Vec<Vec<u8>> = effect
            .passes
            .iter()
            .map(|pass| {
                prelude::encode_host(
                    host,
                    match pass.output {
                        Slot::Out => (width, height),
                        Slot::Target(t) => {
                            let scale = &manifest.targets[t];
                            (
                                ((f64::from(width) * scale.scale).round() as u32).max(1),
                                ((f64::from(height) * scale.scale).round() as u32).max(1),
                            )
                        }
                    },
                    (width, height),
                )
            })
            .collect();
        let staged = self.stage_uniforms(device, queue, &params_bytes, &host_blocks);

        let mut targets: Vec<(wgpu::Texture, wgpu::TextureView)> =
            Vec::with_capacity(manifest.targets.len());
        for t in &manifest.targets {
            let w = ((f64::from(width) * t.scale).round() as u32).max(1);
            let h = ((f64::from(height) * t.scale).round() as u32).max(1);
            let texture = self.take(device, w, h, output_format(t.format));
            let view = texture.create_view(&Default::default());
            targets.push((texture, view));
        }
        let out = match final_target {
            FinalTarget::Output => {
                let texture = self.take(device, width, height, wgpu::TextureFormat::Rgba8Unorm);
                let view = texture.create_view(&Default::default());
                Some((texture, view))
            }
            FinalTarget::Blend(_) => None,
        };

        let ring = self
            .ring
            .as_ref()
            .ok_or_else(|| EffectError::Gpu("uniform ring missing".into()))?;
        for (index, pass) in effect.passes.iter().enumerate() {
            let is_final = index == last;
            let pipeline = &self.pipelines[&(effect.hash, index, blended && is_final)];

            let mut views: Vec<&wgpu::TextureView> = Vec::with_capacity(pass.inputs.len());
            for input in &pass.inputs {
                let view = match input {
                    InputRef::External(name) => *inputs
                        .views
                        .get(name)
                        .ok_or_else(|| EffectError::MissingInput(name.clone()))?,
                    InputRef::Slot(Slot::Target(t)) => &targets[*t].1,
                    InputRef::Slot(Slot::Out) => {
                        return Err(EffectError::Shader {
                            pass: index,
                            message: "the output cannot be read back".into(),
                        })
                    }
                    InputRef::File(f) => &self.files[&(effect.hash, *f)].view,
                };
                views.push(view);
            }
            let group1_entries: Vec<wgpu::BindGroupEntry> = views
                .iter()
                .enumerate()
                .map(|(i, v)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource: wgpu::BindingResource::TextureView(v),
                })
                .collect();
            let group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect-group1"),
                layout: &pipeline.group1,
                entries: &group1_entries,
            });
            let group2_made;
            let group2 = match &pipeline.group2_empty {
                Some(empty) => empty,
                None => {
                    let group2_entries: Vec<wgpu::BindGroupEntry> = pass
                        .inputs
                        .iter()
                        .enumerate()
                        .filter_map(|(i, input)| match input {
                            InputRef::File(f) => Some(wgpu::BindGroupEntry {
                                binding: i as u32,
                                resource: wgpu::BindingResource::Sampler(
                                    &self.files[&(effect.hash, *f)].sampler,
                                ),
                            }),
                            _ => None,
                        })
                        .collect();
                    group2_made = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("effect-group2"),
                        layout: &pipeline.group2,
                        entries: &group2_entries,
                    });
                    &group2_made
                }
            };

            let (target_view, load) = match (pass.output, is_final, &final_target) {
                (Slot::Out, true, FinalTarget::Blend(dest)) => (*dest, wgpu::LoadOp::Load),
                (Slot::Out, _, _) => (
                    out.as_ref()
                        .map(|(_, v)| v)
                        .ok_or_else(|| EffectError::Gpu("no output target".into()))?,
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                ),
                (Slot::Target(t), _, _) => {
                    (&targets[t].1, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT))
                }
            };
            {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("effect-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                rp.set_pipeline(&pipeline.pipeline);
                let host_offset = staged.host_offsets[index];
                rp.set_bind_group(0, &ring.group0, &[host_offset, staged.params_offset]);
                rp.set_bind_group(1, &group1, &[]);
                rp.set_bind_group(2, group2, &[]);
                rp.draw(0..6, 0..1);
            }
        }

        // Intermediates are free once the last pass has been recorded: the encoder keeps them
        // alive until submission, and wgpu orders any reuse after that.
        for (texture, _) in targets {
            self.put(texture);
        }
        Ok(out.map(|(texture, view)| EffectOutput {
            texture,
            view,
            width,
            height,
        }))
    }
}

fn dynamic_uniform_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    min_binding_size: Option<u64>,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: min_binding_size.and_then(wgpu::BufferSize::new),
        },
        count: None,
    }
}

fn sampler_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a(FNV_OFFSET, b""), FNV_OFFSET);
        assert_ne!(fnv1a(FNV_OFFSET, b"a"), fnv1a(FNV_OFFSET, b"b"));
    }

    #[test]
    fn manifest_validation_error_surfaces_before_shaders() {
        let r = CompiledEffect::load("{", &BTreeMap::new(), &BTreeMap::new());
        assert!(matches!(r, Err(EffectError::Manifest(_))));
    }

    #[test]
    fn invalid_wgsl_fails_at_load() {
        let json = r#"{
          "id": "bad", "version": 1, "kind": "effect",
          "passes": [ { "shader": "bad.wgsl", "inputs": ["src"], "output": "out" } ]
        }"#;
        let mut shaders = BTreeMap::new();
        shaders.insert(
            "bad.wgsl".to_string(),
            "@fragment fn fs_main(in: VsOut) -> @location(0) vec4f { return nope; }".to_string(),
        );
        let r = CompiledEffect::load(json, &shaders, &BTreeMap::new());
        assert!(matches!(r, Err(EffectError::Shader { pass: 0, .. })));
    }

    #[test]
    fn missing_shader_is_reported() {
        let json = r#"{
          "id": "missing", "version": 1, "kind": "effect",
          "passes": [ { "shader": "gone.wgsl", "inputs": ["src"], "output": "out" } ]
        }"#;
        let r = CompiledEffect::load(json, &BTreeMap::new(), &BTreeMap::new());
        assert!(matches!(r, Err(EffectError::MissingFile(p)) if p == "gone.wgsl"));
    }

    #[test]
    fn validate_accepts_a_minimal_pass() {
        let prelude = prelude::pass_prelude(
            &UniformLayout::new(&[]),
            &[PreludeInput {
                name: "src".into(),
                lut: false,
                sampler: None,
            }],
        );
        let code = "@fragment fn fs_main(in: VsOut) -> @location(0) vec4f { return textureSample(src, samp_linear, in.uv); }";
        assert!(validate_wgsl(&format!("{prelude}\n{code}"), "fs_main").is_ok());
        assert!(validate_wgsl(&format!("{prelude}\n{code}"), "other").is_err());
    }
}
