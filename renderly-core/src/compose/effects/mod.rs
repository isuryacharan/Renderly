//! Builtin effect registry + GPU ping-pong chain (Phase 3.4).
//!
//! Effects run on each layer's uploaded texture *before* the cover+transform composite
//! draw. Unknown `effect_id`s are rejected at command validation; this module only
//! executes the locked builtins below.

use crate::packs::{cube_lut_upload_bytes, find_cube_lut, parse_pack_lut_id, LoadedPack};
use crate::project::{ClipMask, ClipMaskKind, EffectInstance};
use std::collections::{BTreeMap, HashMap};
use wgpu::util::DeviceExt;

use super::ComposeError;

/// Locked builtin effect ids (also useful for GUI pickers).
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

/// Default params for a builtin (empty map if unknown).
pub fn default_params(effect_id: &str) -> BTreeMap<String, f64> {
    let mut m = BTreeMap::new();
    match effect_id {
        "builtin:color_adjust" => {
            m.insert("exposure".into(), 0.0);
            m.insert("contrast".into(), 1.0);
            m.insert("saturation".into(), 1.0);
        }
        "builtin:blur" => {
            m.insert("radius".into(), 0.0);
        }
        "builtin:lut_contrast" | "builtin:lut_warm" => {
            m.insert("intensity".into(), 1.0);
        }
        "builtin:glitch" => {
            m.insert("intensity".into(), 0.5);
            m.insert("slice".into(), 0.5);
        }
        "builtin:chroma_key" => {
            m.insert("key_r".into(), 0.0);
            m.insert("key_g".into(), 1.0);
            m.insert("key_b".into(), 0.0);
            m.insert("tolerance".into(), 0.3);
            m.insert("softness".into(), 0.1);
            m.insert("spill".into(), 0.0);
        }
        _ => {}
    }
    m
}

/// Clamp known params into finite, reasonable ranges. Unknown keys left unchanged.
pub fn clamp_effect_params(effect_id: &str, params: &mut BTreeMap<String, f64>) {
    for (k, v) in params.iter_mut() {
        *v = match (effect_id, k.as_str()) {
            ("builtin:color_adjust", "exposure") => v.clamp(-5.0, 5.0),
            ("builtin:color_adjust", "contrast") => v.clamp(0.0, 4.0),
            ("builtin:color_adjust", "saturation") => v.clamp(0.0, 4.0),
            ("builtin:blur", "radius") => v.clamp(0.0, 64.0),
            ("builtin:lut_contrast" | "builtin:lut_warm", "intensity") => v.clamp(0.0, 1.0),
            ("builtin:glitch", "intensity") => v.clamp(0.0, 1.0),
            ("builtin:glitch", "slice") => v.clamp(0.0, 1.0),
            ("builtin:chroma_key", "key_r" | "key_g" | "key_b") => v.clamp(0.0, 1.0),
            ("builtin:chroma_key", "tolerance" | "softness" | "spill") => v.clamp(0.0, 1.0),
            _ => *v,
        };
    }
}

fn param_or(params: &BTreeMap<String, f64>, key: &str, default: f64) -> f64 {
    params.get(key).copied().unwrap_or(default)
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
struct ColorAdjustUniform {
    exposure: f32,
    contrast: f32,
    saturation: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniform {
    texel: [f32; 2],
    radius: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LutUniform {
    intensity: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlitchUniform {
    intensity: f32,
    slice: f32,
    time_seed: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PackLutUniform {
    intensity: f32,
    lut_size: f32,
    _pad0: f32,
    _pad1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaKeyUniform {
    key_rgb: [f32; 4],
    tolerance: f32,
    softness: f32,
    spill: f32,
    _pad: f32,
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

enum EffectKind {
    ColorAdjust,
    Blur,
    Lut,
    Glitch,
    ChromaKey,
}

struct PackLutGpu {
    size: u32,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct PingPongRt {
    /// Kept alive so `view` remains valid.
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// GPU resources for the builtin effect chain (owned by [`super::Compositor`]).
pub struct EffectProcessor {
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
    pack_lut_bind_group_layout: wgpu::BindGroupLayout,
    mask_bind_group_layout: wgpu::BindGroupLayout,
    color_adjust_pipeline: wgpu::RenderPipeline,
    blur_pipeline: wgpu::RenderPipeline,
    lut_pipeline: wgpu::RenderPipeline,
    glitch_pipeline: wgpu::RenderPipeline,
    chroma_key_pipeline: wgpu::RenderPipeline,
    pack_lut_pipeline: wgpu::RenderPipeline,
    mask_pipeline: wgpu::RenderPipeline,
    pack_lut_cache: HashMap<String, PackLutGpu>,
    matte_cache: HashMap<String, wgpu::TextureView>,
    dummy_matte_view: Option<wgpu::TextureView>,
    /// Ping/pong render targets per size, so alternating sizes within a frame (a nested
    /// sequence at its own resolution) reuses them instead of reallocating every layer.
    rts: HashMap<(u32, u32), [PingPongRt; 2]>,
    /// Size of the ping/pong pair the last [`Self::apply`] wrote to.
    cur_size: (u32, u32),
    /// After a successful write, index of the RT holding the result (0=ping, 1=pong).
    result_slot: u8,
    unpremul_pipeline: wgpu::RenderPipeline,
    adjust_mix_pipeline: wgpu::RenderPipeline,
    adjust_mix_bind_group_layout: wgpu::BindGroupLayout,
    /// Unused 16-byte uniform for passes that share the single-texture effect layout.
    dummy_uniform: wgpu::Buffer,
}

/// Distinct ping/pong sizes kept alive before the cache is reset.
const MAX_RT_SIZES: usize = 4;

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

        let pack_lut_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("pack-lut-effect"),
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
                            view_dimension: wgpu::TextureViewDimension::D3,
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effect"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pack_lut_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pack-lut-effect"),
                bind_group_layouts: &[Some(&pack_lut_bind_group_layout)],
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

        let color_adjust_pipeline = make_effect_pipeline(
            device,
            &pipeline_layout,
            "color_adjust",
            include_str!("color_adjust.wgsl"),
        );
        let blur_pipeline =
            make_effect_pipeline(device, &pipeline_layout, "blur", include_str!("blur.wgsl"));
        let lut_pipeline =
            make_effect_pipeline(device, &pipeline_layout, "lut", include_str!("lut.wgsl"));
        let glitch_pipeline = make_effect_pipeline(
            device,
            &pipeline_layout,
            "glitch",
            include_str!("glitch.wgsl"),
        );
        let chroma_key_pipeline = make_effect_pipeline(
            device,
            &pipeline_layout,
            "chroma_key",
            include_str!("chroma_key.wgsl"),
        );
        let pack_lut_pipeline = make_effect_pipeline(
            device,
            &pack_lut_pipeline_layout,
            "pack_lut",
            include_str!("pack_lut.wgsl"),
        );
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
            pack_lut_bind_group_layout,
            mask_bind_group_layout,
            color_adjust_pipeline,
            blur_pipeline,
            lut_pipeline,
            glitch_pipeline,
            chroma_key_pipeline,
            pack_lut_pipeline,
            mask_pipeline,
            pack_lut_cache: HashMap::new(),
            matte_cache: HashMap::new(),
            dummy_matte_view: None,
            rts: HashMap::new(),
            cur_size: (0, 0),
            result_slot: 0,
            unpremul_pipeline,
            adjust_mix_pipeline,
            adjust_mix_bind_group_layout,
            dummy_uniform: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("effect-dummy-uniform"),
                contents: &[0u8; 16],
                usage: wgpu::BufferUsages::UNIFORM,
            }),
        }
    }

    fn ensure_rts(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.cur_size = (width, height);
        if self.rts.contains_key(&self.cur_size) {
            return;
        }
        if self.rts.len() >= MAX_RT_SIZES {
            self.rts.clear();
        }
        self.rts.insert(
            self.cur_size,
            [
                create_rt(device, "effect-ping", width, height),
                create_rt(device, "effect-pong", width, height),
            ],
        );
    }

    /// View of ping (0) or pong (1) at the size of the last [`Self::apply`].
    fn slot_view(&self, slot: u8) -> &wgpu::TextureView {
        &self
            .rts
            .get(&self.cur_size)
            .expect("effect RTs for the current size")[slot as usize]
            .view
    }

    pub fn result_view(&self) -> &wgpu::TextureView {
        self.slot_view(self.result_slot)
    }

    /// True when `effects` would write anything (some enabled, resolvable effect).
    pub(super) fn has_work(&self, effects: &[EffectInstance], packs: &[LoadedPack]) -> bool {
        has_enabled_effects(effects, packs)
    }

    /// Run enabled effects on `src_view` into ping-pong RTs. Returns `false` when nothing
    /// was written (caller keeps using `src_view`).
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

        self.ensure_rts(device, width, height);

        let mut read_src = true;
        let mut wrote = false;

        for effect in effects.iter().filter(|e| e.enabled) {
            match effect.effect_id.as_str() {
                "builtin:color_adjust" => {
                    let u = ColorAdjustUniform {
                        exposure: param_or(&effect.params, "exposure", 0.0) as f32,
                        contrast: param_or(&effect.params, "contrast", 1.0) as f32,
                        saturation: param_or(&effect.params, "saturation", 1.0) as f32,
                        _pad: 0.0,
                    };
                    self.draw_pass(
                        device,
                        encoder,
                        src_view,
                        read_src,
                        EffectKind::ColorAdjust,
                        bytemuck::bytes_of(&u),
                    )?;
                    read_src = false;
                    wrote = true;
                }
                "builtin:blur" => {
                    let radius = param_or(&effect.params, "radius", 0.0) as f32;
                    if radius < 0.5 {
                        continue;
                    }
                    let u_h = BlurUniform {
                        texel: [1.0 / width as f32, 0.0],
                        radius,
                        _pad: 0.0,
                    };
                    self.draw_pass(
                        device,
                        encoder,
                        src_view,
                        read_src,
                        EffectKind::Blur,
                        bytemuck::bytes_of(&u_h),
                    )?;
                    read_src = false;
                    wrote = true;

                    let u_v = BlurUniform {
                        texel: [0.0, 1.0 / height as f32],
                        radius,
                        _pad: 0.0,
                    };
                    self.draw_pass(
                        device,
                        encoder,
                        src_view,
                        read_src,
                        EffectKind::Blur,
                        bytemuck::bytes_of(&u_v),
                    )?;
                }
                "builtin:lut_contrast" | "builtin:lut_warm" => {
                    let intensity = param_or(&effect.params, "intensity", 1.0) as f32;
                    if intensity <= 0.0 {
                        continue;
                    }
                    let mode = if effect.effect_id == "builtin:lut_contrast" {
                        0u32
                    } else {
                        1u32
                    };
                    let u = LutUniform {
                        intensity,
                        mode,
                        _pad0: 0.0,
                        _pad1: 0.0,
                    };
                    self.draw_pass(
                        device,
                        encoder,
                        src_view,
                        read_src,
                        EffectKind::Lut,
                        bytemuck::bytes_of(&u),
                    )?;
                    read_src = false;
                    wrote = true;
                }
                "builtin:glitch" => {
                    let intensity = param_or(&effect.params, "intensity", 0.5) as f32;
                    if intensity <= 0.001 {
                        continue;
                    }
                    let u = GlitchUniform {
                        intensity,
                        slice: param_or(&effect.params, "slice", 0.5) as f32,
                        time_seed: param_or(&effect.params, "seed", 0.0) as f32,
                        _pad: 0.0,
                    };
                    self.draw_pass(
                        device,
                        encoder,
                        src_view,
                        read_src,
                        EffectKind::Glitch,
                        bytemuck::bytes_of(&u),
                    )?;
                    read_src = false;
                    wrote = true;
                }
                "builtin:chroma_key" => {
                    let u = ChromaKeyUniform {
                        key_rgb: [
                            param_or(&effect.params, "key_r", 0.0) as f32,
                            param_or(&effect.params, "key_g", 1.0) as f32,
                            param_or(&effect.params, "key_b", 0.0) as f32,
                            0.0,
                        ],
                        tolerance: param_or(&effect.params, "tolerance", 0.3) as f32,
                        softness: param_or(&effect.params, "softness", 0.1) as f32,
                        spill: param_or(&effect.params, "spill", 0.0) as f32,
                        _pad: 0.0,
                    };
                    self.draw_pass(
                        device,
                        encoder,
                        src_view,
                        read_src,
                        EffectKind::ChromaKey,
                        bytemuck::bytes_of(&u),
                    )?;
                    read_src = false;
                    wrote = true;
                }
                id if parse_pack_lut_id(id).is_some() => {
                    let intensity = param_or(&effect.params, "intensity", 1.0) as f32;
                    if intensity <= 0.0 {
                        continue;
                    }
                    let Some((pack_id, lut_id)) = parse_pack_lut_id(id) else {
                        continue;
                    };
                    let Some(cube) = find_cube_lut(packs, pack_id, lut_id) else {
                        continue;
                    };
                    self.ensure_pack_lut_gpu(device, queue, id, cube)?;
                    let lut_view = self
                        .pack_lut_cache
                        .get(id)
                        .expect("pack lut cache")
                        .view
                        .clone();
                    let u = PackLutUniform {
                        intensity,
                        lut_size: cube.size as f32,
                        _pad0: 0.0,
                        _pad1: 0.0,
                    };
                    self.draw_pack_lut_pass(
                        device,
                        encoder,
                        src_view,
                        read_src,
                        &lut_view,
                        bytemuck::bytes_of(&u),
                    )?;
                    read_src = false;
                    wrote = true;
                }
                _ => continue,
            }
        }

        if let Some(mask) = mask.filter(|m| m.enabled) {
            let u = mask_uniform(mask);

            let matte_view = if u.mode == 2 {
                self.ensure_matte_view(device, queue, &mask.kind)?
            } else {
                self.ensure_dummy_matte(device, queue)?
            };

            self.draw_mask_pass(
                device,
                encoder,
                src_view,
                read_src,
                &matte_view,
                bytemuck::bytes_of(&u),
            )?;
            wrote = true;
        }

        Ok(wrote)
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
                    resource: wgpu::BindingResource::TextureView(self.result_view()),
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
        self.pack_lut_cache.clear();
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

    fn draw_mask_pass(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src_view: &wgpu::TextureView,
        read_src: bool,
        matte_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
    ) -> Result<(), ComposeError> {
        let dest_slot = if read_src { 0u8 } else { 1 - self.result_slot };

        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mask-params"),
            contents: uniform_bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let input_is_src = read_src;
        let input_slot = self.result_slot;

        let bind_group = {
            let input_view = if input_is_src {
                src_view
            } else if input_slot == 0 {
                self.slot_view(0)
            } else {
                self.slot_view(1)
            };
            device.create_bind_group(&wgpu::BindGroupDescriptor {
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
            })
        };

        let dest_view = if dest_slot == 0 {
            self.slot_view(0)
        } else {
            self.slot_view(1)
        };

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mask-pass"),
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
            pass.set_pipeline(&self.mask_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        self.result_slot = dest_slot;
        Ok(())
    }

    fn ensure_pack_lut_gpu(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        effect_id: &str,
        cube: &crate::packs::CubeLut,
    ) -> Result<(), ComposeError> {
        let size = cube.size as u32;
        let needs_upload = self
            .pack_lut_cache
            .get(effect_id)
            .map(|e| e.size != size)
            .unwrap_or(true);
        if needs_upload {
            let bytes = cube_lut_upload_bytes(cube);
            let texture = device.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("pack-lut-3d"),
                    size: wgpu::Extent3d {
                        width: size,
                        height: size,
                        depth_or_array_layers: size,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D3,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &bytes,
            );
            let view = texture.create_view(&Default::default());
            self.pack_lut_cache.insert(
                effect_id.to_string(),
                PackLutGpu {
                    size,
                    _texture: texture,
                    view,
                },
            );
        }
        Ok(())
    }

    fn draw_pack_lut_pass(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src_view: &wgpu::TextureView,
        read_src: bool,
        lut_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
    ) -> Result<(), ComposeError> {
        let dest_slot = if read_src { 0u8 } else { 1 - self.result_slot };

        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pack-lut-params"),
            contents: uniform_bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let input_is_src = read_src;
        let input_slot = self.result_slot;

        let bind_group = {
            let input_view = if input_is_src {
                src_view
            } else if input_slot == 0 {
                self.slot_view(0)
            } else {
                self.slot_view(1)
            };
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pack-lut-bind"),
                layout: &self.pack_lut_bind_group_layout,
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
                        resource: wgpu::BindingResource::TextureView(lut_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            })
        };

        let dest_view = if dest_slot == 0 {
            self.slot_view(0)
        } else {
            self.slot_view(1)
        };

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pack-lut-pass"),
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
            pass.set_pipeline(&self.pack_lut_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        self.result_slot = dest_slot;
        Ok(())
    }

    fn draw_pass(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src_view: &wgpu::TextureView,
        read_src: bool,
        kind: EffectKind,
        uniform_bytes: &[u8],
    ) -> Result<(), ComposeError> {
        let dest_slot = if read_src { 0u8 } else { 1 - self.result_slot };

        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("effect-params"),
            contents: uniform_bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });

        // Resolve input/dest views and pipeline without holding conflicting borrows.
        let input_is_src = read_src;
        let input_slot = self.result_slot;
        let pipeline = match kind {
            EffectKind::ColorAdjust => &self.color_adjust_pipeline,
            EffectKind::Blur => &self.blur_pipeline,
            EffectKind::Lut => &self.lut_pipeline,
            EffectKind::Glitch => &self.glitch_pipeline,
            EffectKind::ChromaKey => &self.chroma_key_pipeline,
        };

        let bind_group = {
            let input_view = if input_is_src {
                src_view
            } else if input_slot == 0 {
                self.slot_view(0)
            } else {
                self.slot_view(1)
            };
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect-bind"),
                layout: &self.bind_group_layout,
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
                ],
            })
        };

        let dest_view = if dest_slot == 0 {
            self.slot_view(0)
        } else {
            self.slot_view(1)
        };

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("effect-pass"),
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        self.result_slot = dest_slot;
        Ok(())
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

fn create_rt(device: &wgpu::Device, label: &str, width: u32, height: u32) -> PingPongRt {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    PingPongRt {
        _texture: texture,
        view,
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
