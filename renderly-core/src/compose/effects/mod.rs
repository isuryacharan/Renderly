//! Builtin effect chain (Phase 3.4), now running on the effect runtime.
//!
//! Every builtin effect, filter and transition runs through `compose::runtime`, from the items in
//! `compose::builtins`. This module maps the project's `effect_id`s onto those items, carries
//! the chain's intermediate pictures between passes, and keeps the clip mask and the
//! adjustment-layer mix as compositor code: a clip mask is a slot, not an item (see
//! docs/effect-pack-format.md "Stack slots"). Unknown `effect_id`s are rejected at command
//! validation; this module skips anything it cannot resolve.

use crate::packs::{find_cube_lut, parse_pack_lut_id, LoadedPack};
use crate::project::{ClipMask, ClipMaskKind, EffectInstance};
use std::collections::{BTreeMap, HashMap};
use wgpu::util::DeviceExt;

use super::builtins;
use super::runtime::lut::{upload_cube, Lut3d};
use super::runtime::manifest::ParamKind;
use super::runtime::{
    CompiledEffect, EffectError, EffectExecutor, EffectInputs, EffectOutput, HostValues, ParamValue,
};
use super::ComposeError;

/// Locked builtin effect ids: the v7 `effect_id` strings the project stores. Each names the
/// builtin item of the same name in [`builtins::BUILTINS`] (checked by a unit test).
pub const BUILTIN_EFFECT_IDS: &[&str] = &[
    "builtin:color_adjust",
    "builtin:blur",
    "builtin:lut_contrast",
    "builtin:lut_warm",
    "builtin:glitch",
    "builtin:chroma_key",
];

/// Public list of builtin effect ids for GUI / CLI discovery.
pub fn builtin_effect_ids() -> &'static [&'static str] {
    BUILTIN_EFFECT_IDS
}

pub fn is_builtin_effect_id(effect_id: &str) -> bool {
    BUILTIN_EFFECT_IDS.contains(&effect_id)
}

/// The builtin item behind a locked v7 effect id (`builtin:blur` -> `blur`).
fn builtin_item_for(effect_id: &str) -> Option<&'static str> {
    BUILTIN_EFFECT_IDS
        .iter()
        .find(|id| **id == effect_id)
        .and_then(|id| id.strip_prefix("builtin:"))
}

/// Default params of a builtin, read from its manifest (empty if unknown).
pub fn default_params(effect_id: &str) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let Some(manifest) = builtin_item_for(effect_id).and_then(builtins::manifest) else {
        return out;
    };
    for p in &manifest.params {
        if let ParamValue::Number(n) = p.default {
            out.insert(p.key.clone(), n);
        }
    }
    out
}

/// Clamp known params into their manifest ranges. Unknown keys are left unchanged.
pub fn clamp_effect_params(effect_id: &str, params: &mut BTreeMap<String, f64>) {
    let Some(manifest) = builtin_item_for(effect_id).and_then(builtins::manifest) else {
        return;
    };
    for (key, value) in params.iter_mut() {
        if let Some(spec) = manifest.params.iter().find(|p| &p.key == key) {
            if matches!(
                spec.kind,
                ParamKind::Float | ParamKind::Int | ParamKind::Choice
            ) {
                *value = value.clamp(spec.min, spec.max);
            }
        }
    }
}

fn has_enabled_effects(effects: &[EffectInstance], packs: &[LoadedPack]) -> bool {
    effects.iter().any(|e| {
        if !e.enabled {
            return false;
        }
        if is_builtin_effect_id(&e.effect_id) {
            return true;
        }
        if let Some((pack_id, lut_id)) = parse_pack_lut_id(&e.effect_id) {
            return find_cube_lut(packs, pack_id, lut_id).is_some();
        }
        false
    })
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
}

fn effect_error(e: EffectError) -> ComposeError {
    ComposeError::Effect(e.to_string())
}

/// The compiled builtin `name`, loading it on first use.
pub(super) fn ensure_item<'a>(
    items: &'a mut HashMap<&'static str, CompiledEffect>,
    name: &'static str,
) -> Result<&'a CompiledEffect, ComposeError> {
    if !items.contains_key(name) {
        let compiled = builtins::load_builtin(name).map_err(effect_error)?;
        items.insert(name, compiled);
    }
    items
        .get(name)
        .ok_or_else(|| ComposeError::Effect(format!("builtin '{name}' did not load")))
}

/// Host values of an effect run. Builtins are time-independent; the clip-time values are zero
/// until schema v8 threads them through the stack.
fn effect_host(width: u32, height: u32) -> HostValues {
    HostValues {
        width,
        height,
        time: 0.0,
        duration: 0.0,
        progress: 0.0,
        frame: 0,
        seed: 0,
        speed: 1.0,
    }
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
    /// Runs every builtin item and LUT filter.
    executor: EffectExecutor,
    /// Compiled builtin items, loaded on first use.
    items: HashMap<&'static str, CompiledEffect>,
    /// Uploaded pack LUTs by `effect_id`, re-uploaded when the cube size changes.
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
            include_str!("adjust_mix.wgsl"),
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
            items: HashMap::new(),
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

    /// Run enabled effects on `src_view`, then the clip mask. Returns `false` when nothing was
    /// written (the caller keeps using `src_view`). Passes are recorded in list order; each
    /// intermediate is recycled once the pass that reads it is recorded.
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
    ) -> Result<bool, ComposeError> {
        if !has_enabled_effects(effects, packs) && mask.map(|m| !m.enabled).unwrap_or(true) {
            return Ok(false);
        }

        let mut carry: Option<EffectOutput> = None;
        for effect in effects.iter().filter(|e| e.enabled) {
            let input = carry.as_ref().map_or(src_view, EffectOutput::view);
            let produced =
                self.run_effect(device, queue, encoder, effect, input, width, height, packs)?;
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

    /// Runs one enabled effect instance. `None` when it resolves to nothing or is bypassed
    /// (for example blur at radius 0, or a pack LUT whose file is not loaded).
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
    ) -> Result<Option<EffectOutput>, ComposeError> {
        let lookup = |key: &str| effect.params.get(key).map(|v| ParamValue::Number(*v));
        let host = effect_host(width, height);
        let Self {
            executor,
            items,
            lut_cache,
            ..
        } = self;

        if let Some((pack_id, lut_id)) = parse_pack_lut_id(&effect.effect_id) {
            let Some(cube) = find_cube_lut(packs, pack_id, lut_id) else {
                return Ok(None);
            };
            let stale = lut_cache
                .get(&effect.effect_id)
                .is_none_or(|l| l.size() != cube.size as u32);
            if stale {
                let lut = upload_cube(device, queue, cube).map_err(effect_error)?;
                lut_cache.insert(effect.effect_id.clone(), lut);
            }
            let item = ensure_item(items, "lut_filter")?;
            if item.is_bypassed(&lookup).map_err(effect_error)? {
                return Ok(None);
            }
            let Some(lut) = lut_cache.get(&effect.effect_id) else {
                return Ok(None);
            };
            let mut inputs = EffectInputs::new();
            inputs.bind("src", input).bind("lut", lut.view());
            let out = executor
                .run(device, queue, encoder, item, &inputs, &lookup, &host)
                .map_err(effect_error)?;
            return Ok(Some(out));
        }

        let Some(name) = builtin_item_for(&effect.effect_id) else {
            return Ok(None);
        };
        let item = ensure_item(items, name)?;
        if item.is_bypassed(&lookup).map_err(effect_error)? {
            return Ok(None);
        }
        let mut inputs = EffectInputs::new();
        inputs.bind("src", input);
        let out = executor
            .run(device, queue, encoder, item, &inputs, &lookup, &host)
            .map_err(effect_error)?;
        Ok(Some(out))
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
    use crate::compose::runtime::manifest::ItemKind;

    /// The locked v7 effect ids are exactly the effect and filter builtins' `builtin:<name>`
    /// ids: the list agrees with the manifests.
    #[test]
    fn builtin_effect_ids_agree_with_the_builtin_manifests() {
        for id in BUILTIN_EFFECT_IDS {
            let name = builtin_item_for(id).unwrap_or_else(|| panic!("{id} is not builtin:<name>"));
            let m = builtins::manifest(name).unwrap_or_else(|| panic!("no manifest for {name}"));
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
        assert_eq!(effects, BUILTIN_EFFECT_IDS.len());
    }

    #[test]
    fn default_params_come_from_the_manifests() {
        assert_eq!(default_params("builtin:blur").get("radius"), Some(&0.0));
        assert_eq!(default_params("builtin:glitch").get("seed"), Some(&0.0));
        assert_eq!(
            default_params("builtin:chroma_key").get("key_g"),
            Some(&1.0)
        );
        assert!(default_params("builtin:unknown").is_empty());
    }

    #[test]
    fn clamp_uses_the_manifest_ranges() {
        let mut p = BTreeMap::from([
            ("exposure".to_string(), 9.0),
            ("unknown".to_string(), 100.0),
        ]);
        clamp_effect_params("builtin:color_adjust", &mut p);
        assert_eq!(p["exposure"], 5.0);
        assert_eq!(p["unknown"], 100.0);
    }
}
