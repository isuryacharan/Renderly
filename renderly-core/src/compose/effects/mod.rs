//! The effect stack (Phase 3.4, schema v8 refs and slots), running on the effect runtime.
//!
//! Every effect, filter and transition runs through `compose::runtime`: builtins from
//! `compose::builtins`, pack items from the packs the host passes in. This module resolves an
//! instance's `ref` (`builtin:<id>@<v>`, `pack:<pack>/<item>@<v>`) onto those items, carries the
//! chain's intermediate pictures between passes, and keeps the clip mask and the adjustment-layer
//! mix as compositor code: a clip mask is a slot, not an item (see docs/effect-pack-format.md
//! "Stack slots"). A ref that does not resolve renders pass-through and keeps its params; the
//! caller passes the stack already flattened in render order (filter, key, adjust, `effects[]`)
//! with animated params evaluated.

use crate::packs::status::{resolve_target, Target};
use crate::packs::LoadedPack;
use crate::project::{
    ClipMask, ClipMaskKind, EffectInstance, EffectParam, EffectParams, EffectStatus, Id,
};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

use super::builtins;
use super::runtime::lut::{upload_cube, Lut3d};
use super::runtime::manifest::{ItemKind, ParamKind};
use super::runtime::{
    CompiledEffect, EffectError, EffectExecutor, EffectInputs, EffectManifest, EffectOutput,
    HostValues, ParamValue,
};
use super::{ComposeError, FxTiming};

/// The builtin effect refs: each names the builtin item of the same name in
/// [`builtins::BUILTINS`] at version 1 (checked by a unit test).
pub const BUILTIN_EFFECT_REFS: &[&str] = &[
    "builtin:color_adjust@1",
    "builtin:blur@1",
    "builtin:lut_contrast@1",
    "builtin:lut_warm@1",
    "builtin:glitch@1",
    "builtin:chroma_key@1",
];

/// Public list of builtin effect refs for GUI / CLI discovery.
pub fn builtin_effect_refs() -> &'static [&'static str] {
    BUILTIN_EFFECT_REFS
}

pub fn is_builtin_effect_ref(item_ref: &str) -> bool {
    BUILTIN_EFFECT_REFS.contains(&item_ref)
}

/// The builtin manifest behind a `builtin:<id>@<v>` ref.
fn builtin_manifest_for(item_ref: &str) -> Option<EffectManifest> {
    match crate::packs::refs::ItemRef::parse(item_ref).ok()? {
        crate::packs::refs::ItemRef::Builtin { id, version } => {
            let m = builtins::manifest(&id)?;
            (m.version == version).then_some(m)
        }
        crate::packs::refs::ItemRef::Pack { .. } => None,
    }
}

/// The value of a param as the project stores it.
fn param_to_stored(value: &ParamValue, kind: ParamKind) -> EffectParam {
    match (value, kind) {
        (ParamValue::Number(n), ParamKind::Bool) => EffectParam::Bool(*n != 0.0),
        (ParamValue::Number(n), _) => EffectParam::Number(*n),
        (ParamValue::Vector(v), _) => EffectParam::Vector(v.clone()),
        (ParamValue::Lut(s), _) => EffectParam::Text(s.clone()),
    }
}

/// A manifest's params as stored values: each param's `default` (the identity: what an absent
/// param means) or, with `initial`, the value written when a user or agent adds the effect.
pub fn manifest_params(manifest: &EffectManifest, initial: bool) -> EffectParams {
    manifest
        .params
        .iter()
        .filter(|p| p.kind != ParamKind::Lut)
        .map(|p| {
            let v = if initial { &p.initial } else { &p.default };
            (p.key.clone(), param_to_stored(v, p.kind))
        })
        .collect()
}

/// Default params of a builtin ref, read from its manifest (empty if unknown).
pub fn default_params(item_ref: &str) -> EffectParams {
    builtin_manifest_for(item_ref)
        .map(|m| manifest_params(&m, false))
        .unwrap_or_default()
}

/// Check `params` against `manifest` and clamp them into range: keys must be declared, values
/// must have the declared shape, scalars are clamped to `min..max` (ints and choices rounded),
/// colours to 0..1. Errors name the offending key.
pub fn validate_params(manifest: &EffectManifest, params: &mut EffectParams) -> Result<(), String> {
    for (key, value) in params.iter_mut() {
        let Some(spec) = manifest.params.iter().find(|p| &p.key == key) else {
            let valid: Vec<&str> = manifest.params.iter().map(|p| p.key.as_str()).collect();
            return Err(format!(
                "'{}' has no param '{key}' (valid: {})",
                manifest.id,
                valid.join(", ")
            ));
        };
        if !value.is_finite() {
            return Err(format!("param '{key}' is not finite"));
        }
        let wrong = || {
            format!(
                "param '{key}' of '{}' expects {}",
                manifest.id,
                match spec.kind {
                    ParamKind::Float => "a number",
                    ParamKind::Int | ParamKind::Choice => "an integer",
                    ParamKind::Bool => "true or false",
                    ParamKind::Color => "[r, g, b, a]",
                    ParamKind::Vec2 => "[x, y]",
                    ParamKind::Lut => "a LUT ref string",
                }
            )
        };
        match (spec.kind, &mut *value) {
            (ParamKind::Float, EffectParam::Number(n)) => *n = n.clamp(spec.min, spec.max),
            (ParamKind::Int | ParamKind::Choice, EffectParam::Number(n)) => {
                *n = n.round().clamp(spec.min, spec.max)
            }
            (ParamKind::Bool, EffectParam::Bool(_)) => {}
            (ParamKind::Bool, EffectParam::Number(n)) => *value = EffectParam::Bool(*n != 0.0),
            (ParamKind::Color, EffectParam::Vector(v)) if v.len() == 4 => {
                for c in v.iter_mut() {
                    *c = c.clamp(0.0, 1.0);
                }
            }
            (ParamKind::Vec2, EffectParam::Vector(v)) if v.len() == 2 => {}
            (ParamKind::Lut, EffectParam::Text(_)) => {}
            _ => return Err(wrong()),
        }
    }
    Ok(())
}

/// Clamp known numeric params of a builtin ref into their manifest ranges. Unknown keys and
/// shapes are left unchanged (use [`validate_params`] to reject them).
pub fn clamp_effect_params(item_ref: &str, params: &mut EffectParams) {
    let Some(manifest) = builtin_manifest_for(item_ref) else {
        return;
    };
    for (key, value) in params.iter_mut() {
        if let (Some(spec), EffectParam::Number(n)) =
            (manifest.params.iter().find(|p| &p.key == key), &mut *value)
        {
            if matches!(
                spec.kind,
                ParamKind::Float | ParamKind::Int | ParamKind::Choice
            ) {
                *n = n.clamp(spec.min, spec.max);
            }
        }
    }
}

/// True when at least one enabled instance in `effects` resolves to something this build can
/// run (missing and disabled refs, and `wasm:*` plugins, do not count).
fn has_enabled_effects(effects: &[EffectInstance], packs: &[LoadedPack]) -> bool {
    effects.iter().any(|e| {
        e.enabled
            && !e.is_wasm()
            && resolve_target(packs, &e.item_ref, e.hash.as_deref())
                .target
                .is_some()
    })
}

/// Whether an instance can run: its status and, if it can, its kind (`effect`, `filter`,
/// `lut` or `transition`).
pub fn instance_runs(packs: &[LoadedPack], e: &EffectInstance) -> (EffectStatus, Option<String>) {
    let r = resolve_target(packs, &e.item_ref, e.hash.as_deref());
    let kind = r.target.map(|t| match t {
        Target::Compiled(item) => item.kind.clone(),
        Target::Lut { .. } => "lut".to_string(),
    });
    (r.status, kind)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskUniform {
    bounds: [f32; 4],
    feather: f32,
    invert: f32,
    mode: u32,
    _pad0: f32,
}

/// Matches `MixParams` in adjust_mix.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MixUniform {
    bounds: [f32; 4],
    feather: f32,
    invert: f32,
    mode: u32,
    opacity: f32,
    blend: u32,
    _pad: [u32; 3],
}

fn effect_error(e: EffectError) -> ComposeError {
    ComposeError::Effect(e.to_string())
}

/// Stable per-instance seed: the instance id folded to 32 bits.
pub(super) fn instance_seed(id: Id) -> u32 {
    let v = id.as_u128();
    let folded = v ^ (v >> 64);
    ((folded as u64) ^ ((folded as u64) >> 32)) as u32
}

/// Host values of an effect run.
pub(super) fn effect_host(width: u32, height: u32, timing: &FxTiming, seed: u32) -> HostValues {
    HostValues {
        width,
        height,
        time: timing.time,
        duration: timing.duration,
        progress: if timing.duration > 0.0 {
            (timing.time / timing.duration).clamp(0.0, 1.0)
        } else {
            0.0
        },
        frame: timing.frame,
        seed,
        speed: timing.speed,
    }
}

/// The stored value of an instance's param as the runtime takes it.
pub(super) fn stored_to_runtime(value: &EffectParam) -> ParamValue {
    match value {
        EffectParam::Number(n) => ParamValue::Number(*n),
        EffectParam::Bool(b) => ParamValue::Number(if *b { 1.0 } else { 0.0 }),
        EffectParam::Vector(v) => ParamValue::Vector(v.clone()),
        EffectParam::Text(s) => ParamValue::Lut(s.clone()),
    }
}

fn lut_filter_item() -> Option<std::sync::Arc<CompiledEffect>> {
    crate::packs::builtin_items()
        .iter()
        .find(|i| i.id == "lut_filter" && i.version == 1)
        .and_then(|i| i.compiled.clone())
}

fn lut_cache_key(pack: &str, item: &str, version: u32, hash: &Option<String>) -> String {
    format!(
        "{pack}/{item}@{version}#{}",
        hash.as_deref().unwrap_or_default()
    )
}

fn ensure_lut(
    cache: &mut HashMap<String, Lut3d>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    key: &str,
    cube: &crate::packs::CubeLut,
) -> Result<(), ComposeError> {
    let stale = cache.get(key).is_none_or(|l| l.size() != cube.size as u32);
    if stale {
        let lut = upload_cube(device, queue, cube).map_err(effect_error)?;
        cache.insert(key.to_string(), lut);
    }
    Ok(())
}

/// GPU resources for the effect chain and the compositor-side mask passes (owned by
/// [`super::Compositor`]).
pub struct EffectProcessor {
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
    mask_bind_group_layout: wgpu::BindGroupLayout,
    mask_pipeline: wgpu::RenderPipeline,
    unpremul_pipeline: wgpu::RenderPipeline,
    adjust_mix_pipeline: wgpu::RenderPipeline,
    adjust_mix_bind_group_layout: wgpu::BindGroupLayout,
    /// Unused 16-byte uniform for passes that share the single-texture layout.
    dummy_uniform: wgpu::Buffer,
    /// Runs every builtin and pack item and LUT filter.
    executor: EffectExecutor,
    /// Uploaded pack LUTs by pack, item, version and hash; re-uploaded when the cube size
    /// changes.
    lut_cache: HashMap<String, Lut3d>,
    matte_cache: HashMap<String, wgpu::TextureView>,
    dummy_matte_view: Option<wgpu::TextureView>,
    /// The picture the last [`Self::apply`] produced; the layer draw samples it.
    current: Option<EffectOutput>,
}

impl EffectProcessor {
    pub fn new(device: &wgpu::Device) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("effect-linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // Unpremultiply: 0 = texture, 1 = sampler, 2 = unused uniform.
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effect"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let mask_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mask-effect"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let mask_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask-effect"),
            bind_group_layouts: &[Some(&mask_bind_group_layout)],
            immediate_size: 0,
        });

        let mask_pipeline = make_effect_pipeline(
            device,
            &mask_pipeline_layout,
            "mask",
            include_str!("mask.wgsl"),
        );
        let unpremul_pipeline = make_effect_pipeline(
            device,
            &pipeline_layout,
            "unpremul",
            include_str!("unpremul.wgsl"),
        );

        // adjust_mix: bindings 0/1 = unadjusted/adjusted picture, 2 = sampler, 3 = params,
        // 4 = matte (the dummy 1x1 white matte when no raster mask is in use).
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let adjust_mix_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("adjust-mix"),
                entries: &[
                    tex_entry(0),
                    tex_entry(1),
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    tex_entry(4),
                ],
            });
        let adjust_mix_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("adjust-mix"),
                bind_group_layouts: &[Some(&adjust_mix_bind_group_layout)],
                immediate_size: 0,
            });
        let adjust_mix_pipeline = make_effect_pipeline(
            device,
            &adjust_mix_pipeline_layout,
            "adjust_mix",
            &format!(
                "{}\n{}",
                include_str!("../blend.wgsl"),
                include_str!("adjust_mix.wgsl")
            ),
        );

        Self {
            sampler,
            bind_group_layout,
            mask_bind_group_layout,
            mask_pipeline,
            unpremul_pipeline,
            adjust_mix_pipeline,
            adjust_mix_bind_group_layout,
            dummy_uniform: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("effect-dummy-uniform"),
                contents: &[0u8; 16],
                usage: wgpu::BufferUsages::UNIFORM,
            }),
            executor: EffectExecutor::new(device),
            lut_cache: HashMap::new(),
            matte_cache: HashMap::new(),
            dummy_matte_view: None,
            current: None,
        }
    }

    /// Marks the start of a submitted frame; see [`EffectExecutor::begin_frame`].
    pub(super) fn begin_frame(&mut self) {
        self.executor.begin_frame();
    }

    /// The most recent chain output (valid after [`Self::apply`] returned `true`).
    pub fn result_view(&self) -> Option<&wgpu::TextureView> {
        self.current.as_ref().map(EffectOutput::view)
    }

    /// True when `effects` would write anything (some enabled, resolvable effect).
    pub(super) fn has_work(&self, effects: &[EffectInstance], packs: &[LoadedPack]) -> bool {
        has_enabled_effects(effects, packs)
    }

    /// Run enabled effects on `src_view`, then the clip mask. `effects` is the stack in render
    /// order. Returns `false` when nothing was written (the caller keeps using `src_view`).
    /// Passes are recorded in list order; each intermediate is recycled once the pass that
    /// reads it is recorded.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        src_view: &wgpu::TextureView,
        width: u32,
        height: u32,
        effects: &[EffectInstance],
        mask: Option<&ClipMask>,
        packs: &[LoadedPack],
        timing: &FxTiming,
    ) -> Result<bool, ComposeError> {
        if !has_enabled_effects(effects, packs) && mask.map(|m| !m.enabled).unwrap_or(true) {
            return Ok(false);
        }

        let mut carry: Option<EffectOutput> = None;
        for effect in effects.iter().filter(|e| e.enabled) {
            let input = carry.as_ref().map_or(src_view, EffectOutput::view);
            let produced = self.run_effect(
                device, queue, encoder, effect, input, width, height, packs, timing,
            )?;
            if let Some(out) = produced {
                if let Some(prev) = carry.replace(out) {
                    self.executor.recycle(prev);
                }
            }
        }

        if let Some(mask) = mask.filter(|m| m.enabled) {
            let u = mask_uniform(mask);
            let matte_view = if u.mode == 2 {
                self.ensure_matte_view(device, queue, &mask.kind)?
            } else {
                self.ensure_dummy_matte(device, queue)?
            };
            let input = carry.as_ref().map_or(src_view, EffectOutput::view);
            let out = self.executor.take_output(device, width, height);
            self.draw_mask_pass(
                device,
                encoder,
                input,
                &out,
                &matte_view,
                bytemuck::bytes_of(&u),
            );
            if let Some(prev) = carry.replace(out) {
                self.executor.recycle(prev);
            }
        }

        let wrote = carry.is_some();
        if let Some(last) = carry {
            if let Some(old) = self.current.replace(last) {
                self.executor.recycle(old);
            }
        }
        Ok(wrote)
    }

    /// Runs one enabled effect instance. `None` when it resolves to nothing (missing or
    /// disabled item, a LUT that is not installed, a `wasm:*` plugin) or is bypassed (for example
    /// blur at radius 0): the picture passes through and the instance keeps its params.
    #[allow(clippy::too_many_arguments)]
    fn run_effect(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        effect: &EffectInstance,
        input: &wgpu::TextureView,
        width: u32,
        height: u32,
        packs: &[LoadedPack],
        timing: &FxTiming,
    ) -> Result<Option<EffectOutput>, ComposeError> {
        if effect.is_wasm() {
            return Ok(None);
        }
        let Some(target) = resolve_target(packs, &effect.item_ref, effect.hash.as_deref()).target
        else {
            return Ok(None);
        };
        let lookup = |key: &str| effect.params.get(key).map(stored_to_runtime);
        let host = effect_host(width, height, timing, instance_seed(effect.id));
        let Self {
            executor,
            lut_cache,
            ..
        } = self;

        match target {
            Target::Lut { item, pack, cube } => {
                // A pack LUT is the builtin LUT filter with the LUT bound.
                let Some(filter) = lut_filter_item() else {
                    return Ok(None);
                };
                if filter.is_bypassed(&lookup).map_err(effect_error)? {
                    return Ok(None);
                }
                let key = lut_cache_key(&pack.canonical_id, &item.id, item.version, &item.hash);
                ensure_lut(lut_cache, device, queue, &key, cube)?;
                let Some(lut) = lut_cache.get(&key) else {
                    return Ok(None);
                };
                let mut inputs = EffectInputs::new();
                inputs.bind("src", input).bind("lut", lut.view());
                let out = executor
                    .run(device, queue, encoder, &filter, &inputs, &lookup, &host)
                    .map_err(effect_error)?;
                Ok(Some(out))
            }
            Target::Compiled(item) => {
                let Some(compiled) = item.compiled.as_ref() else {
                    return Ok(None);
                };
                if compiled.manifest().kind == ItemKind::Transition {
                    return Ok(None);
                }
                if compiled.is_bypassed(&lookup).map_err(effect_error)? {
                    return Ok(None);
                }
                // LUT params: every declared LUT must resolve, or the effect passes through.
                let mut lut_keys: Vec<(String, String)> = Vec::new();
                for spec in compiled.manifest().lut_params() {
                    let reference = match effect.params.get(&spec.key) {
                        Some(EffectParam::Text(r)) => r.clone(),
                        _ => match &spec.default {
                            ParamValue::Lut(r) => r.clone(),
                            _ => String::new(),
                        },
                    };
                    let resolved = resolve_target(packs, &reference, None);
                    let Some(Target::Lut { item, pack, cube }) = resolved.target else {
                        return Ok(None);
                    };
                    let key = lut_cache_key(&pack.canonical_id, &item.id, item.version, &item.hash);
                    ensure_lut(lut_cache, device, queue, &key, cube)?;
                    lut_keys.push((spec.key.clone(), key));
                }
                let mut inputs = EffectInputs::new();
                inputs.bind("src", input);
                for (name, key) in &lut_keys {
                    let Some(lut) = lut_cache.get(key) else {
                        return Ok(None);
                    };
                    inputs.bind(name, lut.view());
                }
                let out = executor
                    .run(device, queue, encoder, compiled, &inputs, &lookup, &host)
                    .map_err(effect_error)?;
                Ok(Some(out))
            }
        }
    }

    /// Convert a premultiplied-alpha texture into straight alpha (`dest` = `src` with colour
    /// divided by alpha). Both must be the same size; `dest` is overwritten. Used to hand a
    /// nested sequence's transparent accumulation to the straight-alpha layer path, and to
    /// give adjustment effects straight-alpha input. Exact copy where alpha is 1.
    pub(super) fn unpremultiply(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src_view: &wgpu::TextureView,
        dest_view: &wgpu::TextureView,
    ) -> Result<(), ComposeError> {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unpremul-bind"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.dummy_uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("unpremultiply"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dest_view,
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
        pass.set_pipeline(&self.unpremul_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..6, 0..1);
        Ok(())
    }

    /// Finish an adjustment layer: write `mix(unadjusted, adjusted, opacity x mask)` into
    /// `dest_view` (premultiplied). `unadjusted_view` is the straight-alpha copy of `dest`
    /// that was fed to [`Self::apply`]; the adjusted picture is that call's
    /// [`Self::result_view`], so call this immediately after a successful `apply`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn mix_adjustment(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        unadjusted_view: &wgpu::TextureView,
        dest_view: &wgpu::TextureView,
        opacity: f32,
        mask: Option<&ClipMask>,
        blend: crate::project::BlendMode,
    ) -> Result<(), ComposeError> {
        // A `None`-kind mask means "no restriction" here (there is nothing to restrict by).
        let mask = mask.filter(|m| m.enabled && m.kind != ClipMaskKind::None);
        let m = mask.map(mask_uniform);
        let uniform = MixUniform {
            bounds: m.map(|m| m.bounds).unwrap_or([0.0; 4]),
            feather: m.map(|m| m.feather).unwrap_or(0.0),
            invert: m.map(|m| m.invert).unwrap_or(0.0),
            // 3 = no mask.
            mode: m.map(|m| m.mode).unwrap_or(3),
            opacity,
            blend: blend.shader_index(),
            _pad: [0; 3],
        };
        let matte_view = match (m, mask) {
            (Some(m), Some(mask)) if m.mode == 2 => {
                self.ensure_matte_view(device, queue, &mask.kind)?
            }
            _ => self.ensure_dummy_matte(device, queue)?,
        };
        let adjusted_view = self
            .result_view()
            .cloned()
            .ok_or_else(|| ComposeError::Effect("adjustment has no adjusted picture".into()))?;
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("adjust-mix-params"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("adjust-mix-bind"),
            layout: &self.adjust_mix_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(unadjusted_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&adjusted_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&matte_view),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("adjust-mix"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dest_view,
                resolve_target: None,
                // Every pixel is written (blend off), so the old contents are not needed.
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
        pass.set_pipeline(&self.adjust_mix_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..6, 0..1);
        Ok(())
    }

    fn ensure_matte_view(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        kind: &ClipMaskKind,
    ) -> Result<wgpu::TextureView, ComposeError> {
        let cache_key = match kind {
            ClipMaskKind::Luma { .. } => "luma:live".to_string(), // Don't cache live heuristic mattes
            other => crate::mask::matte_cache_key(other)
                .ok_or_else(|| ComposeError::Wgpu("not a texture mask".into()))?,
        };

        if let Some(view) = self.matte_cache.get(&cache_key) {
            if cache_key != "luma:live" {
                return Ok(view.clone());
            }
        }

        // File-backed mattes miss the cache only when nobody registered them. Native export
        // loads them from disk (`image::open`); wasm32 has no filesystem, so the host must
        // have decoded the file and called `set_matte` first (callers gate on `has_matte`
        // via `Compositor::preview_layer_mask`, so reaching the error arm below is a bug).
        #[cfg(not(target_arch = "wasm32"))]
        let (pixels, width, height) = match kind {
            ClipMaskKind::Raster { .. } | ClipMaskKind::Generated { .. } => {
                let path = crate::mask::matte_file_path(kind)
                    .ok_or_else(|| ComposeError::Wgpu("not a texture mask".into()))?;
                let img = image::open(path).map_err(|e| ComposeError::Wgpu(e.to_string()))?;
                let gray = img.to_luma8();
                let w = gray.width();
                let h = gray.height();
                (gray.into_raw(), w, h)
            }
            ClipMaskKind::Luma {
                pixels,
                width,
                height,
            } => (pixels.clone(), *width, *height),
            _ => unreachable!(),
        };
        #[cfg(target_arch = "wasm32")]
        let (pixels, width, height) = match kind {
            ClipMaskKind::Luma {
                pixels,
                width,
                height,
            } => (pixels.clone(), *width, *height),
            ClipMaskKind::Raster { .. } | ClipMaskKind::Generated { .. } => {
                return Err(ComposeError::Wgpu(
                    "matte not registered: the host must decode the matte file and call                      Compositor::set_matte before compositing a raster/generated mask"
                        .into(),
                ));
            }
            _ => unreachable!(),
        };

        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("matte"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &pixels,
        );
        let view = texture.create_view(&Default::default());
        if cache_key != "luma:live" {
            self.matte_cache.insert(cache_key, view.clone());
        }
        Ok(view)
    }

    /// Register (or replace) the luma matte stored under `key` (see
    /// [`crate::mask::matte_cache_key`]). `luma` is row-major, unpadded, `width * height`
    /// bytes. This is the filesystem-free entry point: the wasm preview decodes the matte
    /// file in the webview and hands pixels here; masks of the matching kind then resolve to
    /// this texture instead of `image::open`.
    pub(super) fn set_matte(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: &str,
        luma: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), ComposeError> {
        if width == 0 || height == 0 || luma.len() != width as usize * height as usize {
            return Err(ComposeError::Wgpu(format!(
                "matte '{key}': {} bytes does not match {width}x{height} luma",
                luma.len()
            )));
        }
        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("matte"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            luma,
        );
        self.matte_cache
            .insert(key.to_string(), texture.create_view(&Default::default()));
        Ok(())
    }

    pub(super) fn has_matte_key(&self, key: &str) -> bool {
        self.matte_cache.contains_key(key)
    }

    pub(super) fn remove_matte(&mut self, key: &str) {
        self.matte_cache.remove(key);
    }

    /// Drop uploaded pack-LUT 3D textures so the next use re-uploads from the (possibly
    /// replaced) `LoadedPack` data - the cache is otherwise keyed by effect id + size only.
    pub(super) fn clear_pack_lut_cache(&mut self) {
        self.lut_cache.clear();
    }

    fn ensure_dummy_matte(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<wgpu::TextureView, ComposeError> {
        if let Some(view) = &self.dummy_matte_view {
            return Ok(view.clone());
        }
        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("dummy-matte"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[255u8],
        );
        let view = texture.create_view(&Default::default());
        self.dummy_matte_view = Some(view.clone());
        Ok(view)
    }

    /// Clip-mask pass: `input_view` masked by the matte into `dest`.
    fn draw_mask_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input_view: &wgpu::TextureView,
        dest: &EffectOutput,
        matte_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
    ) {
        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mask-params"),
            contents: uniform_bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask-bind"),
            layout: &self.mask_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(matte_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mask-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dest.view(),
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
        pass.set_pipeline(&self.mask_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// Shader parameters for `mask.wgsl` / `adjust_mix.wgsl` (mode 0 rect, 1 ellipse, 2 raster).
fn mask_uniform(mask: &ClipMask) -> MaskUniform {
    match &mask.kind {
        ClipMaskKind::None => MaskUniform {
            bounds: [0.0; 4],
            feather: 0.0,
            invert: 0.0,
            mode: 0,
            _pad0: 0.0,
        },
        ClipMaskKind::Rect {
            x,
            y,
            width,
            height,
        } => MaskUniform {
            bounds: [*x as f32, *y as f32, *width as f32, *height as f32],
            feather: mask.feather as f32,
            invert: if mask.invert { 1.0 } else { 0.0 },
            mode: 0,
            _pad0: 0.0,
        },
        ClipMaskKind::Ellipse { cx, cy, rx, ry } => MaskUniform {
            bounds: [*cx as f32, *cy as f32, *rx as f32, *ry as f32],
            feather: mask.feather as f32,
            invert: if mask.invert { 1.0 } else { 0.0 },
            mode: 1,
            _pad0: 0.0,
        },
        ClipMaskKind::Raster { .. }
        | ClipMaskKind::Generated { .. }
        | ClipMaskKind::Luma { .. } => MaskUniform {
            bounds: [0.0; 4],
            feather: mask.feather as f32,
            invert: if mask.invert { 1.0 } else { 0.0 },
            mode: 2,
            _pad0: 0.0,
        },
    }
}

fn make_effect_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    label: &str,
    wgsl: &str,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The builtin effect refs are exactly the effect and filter builtins at version 1: the
    /// list agrees with the manifests.
    #[test]
    fn builtin_effect_refs_agree_with_the_builtin_manifests() {
        for id in BUILTIN_EFFECT_REFS {
            let m = builtin_manifest_for(id).unwrap_or_else(|| panic!("{id} is not a builtin ref"));
            assert!(
                matches!(m.kind, ItemKind::Effect | ItemKind::Filter),
                "{id}"
            );
        }
        let effects = builtins::BUILTINS
            .iter()
            .filter(|b| {
                builtins::manifest(b.name).is_some_and(|m| matches!(m.kind, ItemKind::Effect))
            })
            .count();
        assert_eq!(effects, BUILTIN_EFFECT_REFS.len());
    }

    #[test]
    fn default_params_come_from_the_manifests() {
        assert_eq!(
            default_params("builtin:blur@1").get("radius"),
            Some(&EffectParam::Number(0.0))
        );
        assert_eq!(
            default_params("builtin:glitch@1").get("seed"),
            Some(&EffectParam::Number(0.0))
        );
        assert_eq!(
            default_params("builtin:chroma_key@1").get("key_g"),
            Some(&EffectParam::Number(1.0))
        );
        assert!(default_params("builtin:unknown@1").is_empty());
        assert!(
            default_params("builtin:blur").is_empty(),
            "refs carry @version"
        );
    }

    #[test]
    fn clamp_uses_the_manifest_ranges() {
        let mut p = BTreeMap::from([
            ("exposure".to_string(), EffectParam::Number(9.0)),
            ("unknown".to_string(), EffectParam::Number(100.0)),
        ]);
        clamp_effect_params("builtin:color_adjust@1", &mut p);
        assert_eq!(p["exposure"], EffectParam::Number(5.0));
        assert_eq!(p["unknown"], EffectParam::Number(100.0));
    }

    #[test]
    fn validate_params_rejects_unknown_keys_and_wrong_shapes() {
        let m = builtins::manifest("color_adjust").unwrap();
        let mut ok = BTreeMap::from([("exposure".to_string(), EffectParam::Number(9.0))]);
        validate_params(&m, &mut ok).unwrap();
        assert_eq!(ok["exposure"], EffectParam::Number(5.0));
        let mut unknown = BTreeMap::from([("nope".to_string(), EffectParam::Number(1.0))]);
        assert!(validate_params(&m, &mut unknown)
            .unwrap_err()
            .contains("no param 'nope'"));
        let mut wrong = BTreeMap::from([("exposure".to_string(), EffectParam::Vector(vec![1.0]))]);
        assert!(validate_params(&m, &mut wrong)
            .unwrap_err()
            .contains("expects a number"));
    }

    #[test]
    fn instance_seed_is_stable_per_id() {
        let id = Id::new_v4();
        assert_eq!(instance_seed(id), instance_seed(id));
    }
}
