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

use wgpu::util::DeviceExt;

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
    group0: wgpu::BindGroupLayout,
    group1: wgpu::BindGroupLayout,
    group2: wgpu::BindGroupLayout,
}

struct FileTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

/// Runs compiled effects on one device. Create one per device and keep it.
pub struct EffectExecutor {
    sampler_linear: wgpu::Sampler,
    sampler_nearest: wgpu::Sampler,
    pipelines: HashMap<(u64, usize), PassPipeline>,
    files: HashMap<(u64, usize), FileTexture>,
    pool: HashMap<(u32, u32, wgpu::TextureFormat), Vec<wgpu::Texture>>,
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
        Self {
            sampler_linear,
            sampler_nearest,
            pipelines: HashMap::new(),
            files: HashMap::new(),
            pool: HashMap::new(),
        }
    }

    /// Returns a finished output to the pool.
    pub fn recycle(&mut self, out: EffectOutput) {
        self.put(out.texture);
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

    fn ensure_pipeline(&mut self, device: &wgpu::Device, effect: &CompiledEffect, index: usize) {
        let key = (effect.hash, index);
        if self.pipelines.contains_key(&key) {
            return;
        }
        let pass = &effect.passes[index];
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let group0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect-group0"),
            entries: &[
                uniform_entry(0, fragment),
                uniform_entry(1, fragment),
                sampler_entry(2, fragment),
                sampler_entry(3, fragment),
            ],
        });
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effect-pipeline-layout"),
            bind_group_layouts: &[Some(&group0), Some(&group1), Some(&group2)],
            immediate_size: 0,
        });
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
                    blend: None,
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
                group0,
                group1,
                group2,
            },
        );
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

    /// Runs `effect` once and returns its output. `params` are the stored control values
    /// (missing keys take their defaults). The effect's input frame is `host.width` x
    /// `host.height`; intermediate targets scale from it. The caller submits `encoder`.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        effect: &CompiledEffect,
        inputs: &EffectInputs<'_>,
        params: &BTreeMap<String, ParamValue>,
        host: &HostValues,
    ) -> Result<EffectOutput, EffectError> {
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

        for index in 0..effect.passes.len() {
            self.ensure_pipeline(device, effect, index);
        }
        for index in 0..manifest.textures.len() {
            self.ensure_file(device, queue, effect, index);
        }

        let params_buffer = uniform_buffer(device, &params_bytes);
        let (width, height) = (host.width, host.height);
        let mut targets: Vec<(wgpu::Texture, wgpu::TextureView)> =
            Vec::with_capacity(manifest.targets.len());
        for t in &manifest.targets {
            let w = ((f64::from(width) * t.scale).round() as u32).max(1);
            let h = ((f64::from(height) * t.scale).round() as u32).max(1);
            let texture = self.take(device, w, h, output_format(t.format));
            let view = texture.create_view(&Default::default());
            targets.push((texture, view));
        }
        let out_texture = self.take(device, width, height, wgpu::TextureFormat::Rgba8Unorm);
        let out_view = out_texture.create_view(&Default::default());

        for (index, pass) in effect.passes.iter().enumerate() {
            let pipeline = &self.pipelines[&(effect.hash, index)];
            let host_bytes = prelude::encode_host(
                host,
                match pass.output {
                    Slot::Out => (width, height),
                    Slot::Target(t) => (targets[t].0.width(), targets[t].0.height()),
                },
                (width, height),
            );
            let host_buffer = uniform_buffer(device, &host_bytes);

            let group0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect-group0"),
                layout: &pipeline.group0,
                entries: &[
                    buffer_entry(0, &host_buffer),
                    buffer_entry(1, &params_buffer),
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
            let group2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect-group2"),
                layout: &pipeline.group2,
                entries: &group2_entries,
            });

            let target_view = match pass.output {
                Slot::Out => &out_view,
                Slot::Target(t) => &targets[t].1,
            };
            {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("effect-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
                rp.set_bind_group(0, &group0, &[]);
                rp.set_bind_group(1, &group1, &[]);
                rp.set_bind_group(2, &group2, &[]);
                rp.draw(0..6, 0..1);
            }
        }

        // Intermediates are free once the last pass has been recorded: the encoder keeps them
        // alive until submission, and wgpu orders any reuse after that.
        for (texture, _) in targets {
            self.put(texture);
        }
        Ok(EffectOutput {
            texture: out_texture,
            view: out_view,
            width,
            height,
        })
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
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

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

/// A uniform buffer holding `bytes`.
fn uniform_buffer(device: &wgpu::Device, bytes: &[u8]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("effect-uniform"),
        contents: bytes,
        usage: wgpu::BufferUsages::UNIFORM,
    })
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
