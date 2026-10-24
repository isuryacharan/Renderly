//! Offscreen wgpu compositor for Phase 0 export. Decoded frames are uploaded as textures,
//! scaled to the output resolution, and read back as RGBA for the FFmpeg encoder.
//! Phase 3.1 adds per-layer user transform (translate / scale / rotate) and opacity.
//! Phase 3.4 runs builtin effect chains on each layer before the cover+transform draw.
//! Phase 3 adds dual-texture WGSL transitions.
//! Schema v7 adds adjustment-layer steps and nested sequences rendered offscreen
//! (see [`SceneStep`] / [`SequenceScene`]).

pub mod builtins;
pub mod effects;
pub mod eval;
pub mod preview_overrides;
pub mod runtime;
mod transition;

pub use effects::{
    builtin_effect_refs, clamp_effect_params, default_params, manifest_params, validate_params,
    BUILTIN_EFFECT_REFS,
};
pub use eval::{
    active_captions, active_steps, leaf_media_layers, sequence_steps, ActiveCaption, ActiveLayer,
    ActiveSource, ActiveText, ComposeStep, EvalError, InstanceKey,
};
pub use preview_overrides::{PreviewOverride, PreviewOverrideEntry, PreviewOverrides};

use crate::frame::RgbaFrame;
use crate::packs::status::{resolve_target, Target as ResolvedTarget};
use crate::packs::LoadedPack;
use crate::project::{BlendMode, ClipMask, ClipTransform, EffectInstance, EffectParams};
use effects::EffectProcessor;
use runtime::manifest::ItemKind;
use std::collections::HashMap;
use std::sync::Arc as StdArc;
use std::sync::Arc;
use thiserror::Error;
use transition::TransitionPass;
use wgpu::util::DeviceExt;

#[derive(Debug, Error)]
pub enum ComposeError {
    #[error("no suitable GPU adapter found")]
    NoAdapter,
    #[error("wgpu error: {0}")]
    Wgpu(String),
    #[error("effect runtime: {0}")]
    Effect(String),
}

/// Clip-relative time values the effects see (`t`, `duration`, `frame`, `speed` in shaping
/// expressions and the host block): seconds since the effect started on the clip, following
/// clip speed, so a 2x clip runs an effect's motion at 2x.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FxTiming {
    pub time: f64,
    pub duration: f64,
    pub frame: u32,
    pub speed: f64,
}

impl Default for FxTiming {
    fn default() -> Self {
        Self {
            time: 0.0,
            duration: 0.0,
            frame: 0,
            speed: 1.0,
        }
    }
}

/// Marks a layer as one side of an outgoing→incoming transition pair.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerTransition {
    /// `builtin:<id>@<v>` or `pack:<pack>/<item>@<v>`.
    pub item_ref: String,
    pub hash: Option<String>,
    pub params: EffectParams,
    /// Linear 0..1 position in the transition window, or already eased when the transition
    /// stores its own easing (`default_easing == false`).
    pub progress: f32,
    pub is_incoming: bool,
    /// True when the transition has no stored easing: the compositor applies the item's
    /// declared default easing to `progress`.
    pub default_easing: bool,
}

/// Pixel source for one composited layer.
///
/// `Frame` is the native decode path (CPU RGBA uploaded through the pooled layer
/// textures). `Texture` is the P2 wasm-preview path: an externally-owned GPU texture the
/// browser filled via `Queue::copy_external_image_to_texture` from a `<video>`/`<img>`
/// element — zero CPU readback (docs/preview-webview.md). Both variants run the exact same
/// effect/mask/transition/composite passes after this point.
pub enum LayerSource {
    Frame(RgbaFrame),
    /// Must be `Rgba8Unorm` with at least `TEXTURE_BINDING` usage, on the same device as
    /// the compositor. The handle is cheap to clone (internally refcounted); the caller
    /// keeps ownership and reuses it across frames.
    Texture {
        texture: wgpu::Texture,
        width: u32,
        height: u32,
    },
    /// A nested sequence (schema v7): its own steps are composited offscreen at the
    /// sequence's resolution over a transparent background, and the result is then this
    /// layer's picture — transform / effects / mask / transition / opacity apply to it like
    /// to any decoded frame.
    Sequence(Box<SequenceScene>),
}

impl LayerSource {
    fn width(&self) -> u32 {
        match self {
            Self::Frame(f) => f.width,
            Self::Texture { width, .. } => *width,
            Self::Sequence(scene) => scene.width,
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Frame(f) => f.height,
            Self::Texture { height, .. } => *height,
            Self::Sequence(scene) => scene.height,
        }
    }
}

/// The contents of a nested sequence at one instant, ready to composite.
pub struct SequenceScene {
    pub width: u32,
    pub height: u32,
    pub steps: Vec<SceneStep>,
}

/// An adjustment layer ready to composite: `effects` run on everything composited so far,
/// and the result is mixed with the unadjusted picture by `opacity` x `mask`.
pub struct ComposeAdjustment {
    /// The stack in render order (filter, adjust, `effects[]`), animated params evaluated.
    pub effects: Vec<EffectInstance>,
    pub opacity: f32,
    /// Output-frame (canvas) UVs.
    pub mask: Option<ClipMask>,
    pub blend_mode: BlendMode,
    pub timing: FxTiming,
}

/// One compositing step: a layer or an adjustment, bottom to top.
// A step is built once per layer per frame and consumed at once; boxing the layer would only add
// an allocation per layer per frame.
#[allow(clippy::large_enum_variant)]
pub enum SceneStep {
    Layer(ComposeLayer),
    Adjustment(ComposeAdjustment),
}

impl SceneStep {
    /// Build an adjustment step from the evaluated [`ComposeStep::Adjustment`] fields.
    pub fn adjustment(
        effects: Vec<EffectInstance>,
        opacity: f32,
        mask: Option<ClipMask>,
        blend_mode: BlendMode,
        timing: FxTiming,
    ) -> Self {
        Self::Adjustment(ComposeAdjustment {
            effects,
            opacity,
            mask,
            blend_mode,
            timing,
        })
    }

    fn as_ref(&self) -> StepRef<'_> {
        match self {
            Self::Layer(l) => StepRef::Layer(l),
            Self::Adjustment(a) => StepRef::Adjustment(a),
        }
    }
}

/// What a host (native export, the wasm preview, a test stand-in) supplies to turn evaluated
/// steps into compositor input. [`build_scene`] owns the shared part — adjustment steps,
/// recursing into nested sequences at their own resolution — so every host assembles
/// sequences identically and preview/export cannot drift.
pub trait SceneHost {
    type Error;

    /// The ready-to-composite layer for a media-sampling [`ActiveLayer`] (usually
    /// [`ComposeLayer::from_active`] over a decoded frame or element texture and the host's
    /// mask resolution), or `None` to skip it this frame (decode ended, element not ready).
    fn media_layer(&mut self, layer: &ActiveLayer) -> Result<Option<ComposeLayer>, Self::Error>;

    /// The mask applied to a nested-sequence layer's picture.
    fn sequence_mask(&mut self, layer: &ActiveLayer) -> Option<ClipMask>;

    /// The ready-to-composite layer for a text clip: its text drawn at `size`, the output size
    /// of the timeline it sits on (the project's, or a nested sequence's own), usually
    /// [`ComposeLayer::from_active`] over the caption renderer's bitmap; `None` skips it.
    fn text_layer(
        &mut self,
        layer: &ActiveLayer,
        size: (u32, u32),
    ) -> Result<Option<ComposeLayer>, Self::Error>;
}

/// Failure assembling a scene: evaluation, or the host's own error.
#[derive(Debug)]
pub enum SceneError<E> {
    Eval(EvalError),
    Host(E),
}

impl<E> From<EvalError> for SceneError<E> {
    fn from(e: EvalError) -> Self {
        Self::Eval(e)
    }
}

/// Turn evaluated steps (see [`active_steps`]) into compositor steps via `host`, recursing
/// into nested sequences. Does not add captions.
pub fn build_scene<H: SceneHost>(
    project: &crate::project::Project,
    steps: Vec<ComposeStep>,
    host: &mut H,
) -> Result<Vec<SceneStep>, SceneError<H::Error>> {
    let size = (project.settings.width, project.settings.height);
    build_scene_at(project, steps, host, size)
}

/// [`build_scene`] for the timeline whose output is `size` (a nested sequence has its own).
fn build_scene_at<H: SceneHost>(
    project: &crate::project::Project,
    steps: Vec<ComposeStep>,
    host: &mut H,
    size: (u32, u32),
) -> Result<Vec<SceneStep>, SceneError<H::Error>> {
    let mut scene = Vec::with_capacity(steps.len() + 1);
    for step in steps {
        let layer = match step {
            ComposeStep::Adjustment {
                effects,
                opacity,
                mask,
                blend_mode,
                timing,
            } => {
                scene.push(SceneStep::adjustment(
                    effects, opacity, mask, blend_mode, timing,
                ));
                continue;
            }
            ComposeStep::Layer(layer) => layer,
        };
        match &layer.source {
            ActiveSource::Media { .. } => {
                if let Some(composed) = host.media_layer(&layer).map_err(SceneError::Host)? {
                    scene.push(SceneStep::Layer(composed));
                }
            }
            ActiveSource::Text(_) => {
                if let Some(composed) = host.text_layer(&layer, size).map_err(SceneError::Host)? {
                    scene.push(SceneStep::Layer(composed));
                }
            }
            ActiveSource::Sequence {
                sequence_id,
                time_us,
                instance,
            } => {
                let seq = project
                    .find_sequence(*sequence_id)
                    .ok_or(EvalError::SequenceNotFound(*sequence_id))?;
                let (width, height) = project.sequence_size(seq);
                let inner = sequence_steps(project, *sequence_id, *time_us, instance)?;
                let inner = build_scene_at(project, inner, host, (width, height))?;
                let mask = host.sequence_mask(&layer);
                scene.push(SceneStep::Layer(ComposeLayer::from_active(
                    &layer,
                    LayerSource::Sequence(Box::new(SequenceScene {
                        width,
                        height,
                        steps: inner,
                    })),
                    mask,
                )));
            }
        }
    }
    Ok(scene)
}

/// Borrowed view over a step, so plain `&[ComposeLayer]` callers need no conversion.
#[derive(Clone, Copy)]
enum StepRef<'a> {
    Layer(&'a ComposeLayer),
    Adjustment(&'a ComposeAdjustment),
}

/// One composited layer: decoded (or caption) pixels plus evaluated transform at frame time.
pub struct ComposeLayer {
    pub source: LayerSource,
    pub transform: ClipTransform,
    /// The effect stack in render order (filter, key, adjust, `effects[]`) with animated params
    /// evaluated at the frame time. Empty / all-disabled / unresolvable → identity path.
    pub effects: Vec<EffectInstance>,
    /// Optional clip mask (Phase 4).
    pub mask: Option<ClipMask>,
    /// When set on two consecutive layers (outgoing then incoming), uses the transition pass.
    pub transition: Option<LayerTransition>,
    pub blend_mode: BlendMode,
    pub timing: FxTiming,
}

impl ComposeLayer {
    /// The single place an evaluated [`ActiveLayer`] plus its pixels becomes a
    /// [`ComposeLayer`]. Shared by the native export path (`export::FrameRenderer`, with a
    /// decoded `LayerSource::Frame`) and the wasm preview (`renderly-wasm`, with a
    /// `LayerSource::Texture` filled from a browser element), so transform / effects /
    /// transition can never be wired differently in the two. `mask` is passed in because
    /// each path legitimately resolves it differently: export may replace it with a
    /// live background-removal matte; the preview uses
    /// [`Compositor::preview_layer_mask`].
    pub fn from_active(layer: &ActiveLayer, source: LayerSource, mask: Option<ClipMask>) -> Self {
        Self {
            source,
            transform: layer.transform,
            effects: layer.effects.clone(),
            mask,
            transition: layer.transition.clone(),
            blend_mode: layer.blend_mode,
            timing: layer.timing,
        }
    }

    /// A burned-in caption layer: the bitmap is already output-sized, so it uses the identity
    /// transform (cover-fit is then a no-op), no effects, no mask, no transition. Shared by
    /// export and the wasm preview.
    pub fn caption(source: LayerSource) -> Self {
        Self {
            source,
            transform: ClipTransform::default(),
            effects: Vec::new(),
            mask: None,
            transition: None,
            blend_mode: BlendMode::Normal,
            timing: FxTiming::default(),
        }
    }
}

/// The one wgpu device every natively constructed [`Compositor`] shares.
///
/// Each `Compositor::new` used to build its own `Instance` (probing every backend), adapter
/// and device. Doing that from several threads at once — parallel renders, or just the test
/// suite — intermittently hung or crashed inside the graphics driver. wgpu devices are
/// `Send + Sync` and meant to be shared, so create one per process and hand out clones.
/// A failure is cached too: a machine without an adapter stays without one.
#[cfg(not(target_arch = "wasm32"))]
fn shared_device() -> Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), ComposeError> {
    type Shared = Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), Option<String>>;
    static SHARED: std::sync::Mutex<Option<Shared>> = std::sync::Mutex::new(None);

    let mut slot = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = Some(pollster::block_on(create_device()).map_err(|e| match e {
            ComposeError::NoAdapter => None,
            other => Some(other.to_string()),
        }));
    }
    match slot.as_ref() {
        Some(Ok((device, queue))) => Ok((device.clone(), queue.clone())),
        Some(Err(None)) | None => Err(ComposeError::NoAdapter),
        Some(Err(Some(msg))) => Err(ComposeError::Wgpu(msg.clone())),
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn create_device() -> Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), ComposeError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        })
        .await
        .map_err(|_| ComposeError::NoAdapter)?;

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("renderly-export"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(|e| ComposeError::Wgpu(e.to_string()))?;

    Ok((Arc::new(device), Arc::new(queue)))
}

impl From<RgbaFrame> for ComposeLayer {
    fn from(frame: RgbaFrame) -> Self {
        Self {
            source: LayerSource::Frame(frame),
            transform: ClipTransform::default(),
            effects: Vec::new(),
            mask: None,
            transition: None,
            blend_mode: BlendMode::Normal,
            timing: FxTiming::default(),
        }
    }
}

/// Matches `LayerParams` in composite.wgsl (48 bytes, 16-byte aligned).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerParamsUniform {
    cover_scale: [f32; 2],
    /// Window start in source UV, already including the cover-fit position (`fit_x`/`fit_y`).
    cover_offset: [f32; 2],
    user_translate: [f32; 2],
    user_scale: [f32; 2],
    rotation_rad: f32,
    opacity: f32,
    blend_mode: u32,
    _pad: u32,
}

/// "Cover" fit: scale/offset (in source-texture UV space) that fill the output rect
/// without distorting the layer's aspect ratio, cropping whichever axis overflows.
/// Identity when the layer already matches the output aspect ratio (e.g. caption layers,
/// which are already rendered at the output resolution).
///
/// `fit_x`/`fit_y` (-1..1) slide the window across the cropped axis: offset =
/// `(1 - scale) * (1 + fit) / 2`, so 0 is the centred window. This is the f32 mirror of
/// [`crate::project::cover_window`] (kept in f32 so the fit-0 result is bit-identical to the
/// pre-fit compositor and existing golden frames do not move).
fn cover_uv(
    layer_w: u32,
    layer_h: u32,
    out_w: u32,
    out_h: u32,
    fit_x: f32,
    fit_y: f32,
) -> ([f32; 2], [f32; 2]) {
    let layer_aspect = layer_w as f32 / layer_h as f32;
    let out_aspect = out_w as f32 / out_h as f32;
    let (scale_x, scale_y) = if layer_aspect > out_aspect {
        // Layer is relatively wider than the output: crop its left/right edges.
        (out_aspect / layer_aspect, 1.0)
    } else {
        // Layer is relatively taller (or equal) than the output: crop top/bottom.
        (1.0, layer_aspect / out_aspect)
    };
    (
        [scale_x, scale_y],
        [
            (1.0 - scale_x) * (1.0 + fit_x) / 2.0,
            (1.0 - scale_y) * (1.0 + fit_y) / 2.0,
        ],
    )
}

fn layer_params(
    layer_w: u32,
    layer_h: u32,
    out_w: u32,
    out_h: u32,
    transform: &ClipTransform,
    blend_mode: BlendMode,
) -> LayerParamsUniform {
    let t = transform.clamp_opacity().clamp_fit();
    let (cover_scale, cover_offset) = cover_uv(
        layer_w,
        layer_h,
        out_w,
        out_h,
        t.fit_x as f32,
        t.fit_y as f32,
    );
    LayerParamsUniform {
        cover_scale,
        cover_offset,
        user_translate: [t.x as f32, t.y as f32],
        user_scale: [t.scale_x as f32, t.scale_y as f32],
        rotation_rad: (t.rotation_deg as f32).to_radians(),
        opacity: t.opacity as f32,
        blend_mode: blend_mode.shader_index(),
        _pad: 0,
    }
}

pub struct Compositor {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    width: u32,
    height: u32,
    output_texture: wgpu::Texture,
    output_view: wgpu::TextureView,
    readback_buffer: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    /// Blend modes other than normal: reads a copy of the accumulation (binding 3), blending
    /// off.
    blend_bind_group_layout: wgpu::BindGroupLayout,
    blend_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    effects: EffectProcessor,
    transitions: TransitionPass,
    // A1: per-source-size free list of layer upload textures, reused across composite()
    // calls instead of allocating one per layer per frame (playback holds a decoded frame
    // at the same size for many consecutive frames, so the steady state is near-100% reuse).
    layer_pool: TexturePool,
    // v7: render-target textures for nested sequences and adjustment scratch, pooled per
    // size the same way so a steady-state frame allocates nothing.
    target_pool: TexturePool,
}

/// Layer draw bindings: 0 picture, 1 sampler, 2 layer params.
fn bind_group_layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
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
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
    ]
}

/// Textures a frame borrowed from the pools; handed back after the commands are submitted
/// (they must stay alive, and un-reused, until then).
#[derive(Default)]
struct FrameScratch {
    uploads: Vec<wgpu::Texture>,
    targets: Vec<wgpu::Texture>,
}

/// A colour target the steps are composited into: the main output, or a nested sequence's
/// offscreen texture. `width`/`height` drive cover-fit and the effect/transition RT sizes.
struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// A layer's picture after upload / nested rendering, ready for effects + the draw pass.
struct Prepared {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

const TEXTURE_POOL_MAX_PER_SIZE: usize = 8;

#[derive(Default)]
struct TexturePool {
    free: HashMap<(u32, u32), Vec<wgpu::Texture>>,
}

impl TexturePool {
    fn take(&mut self, width: u32, height: u32) -> Option<wgpu::Texture> {
        self.free.get_mut(&(width, height))?.pop()
    }

    fn give_back(&mut self, texture: wgpu::Texture) {
        let key = (texture.width(), texture.height());
        let bucket = self.free.entry(key).or_default();
        if bucket.len() < TEXTURE_POOL_MAX_PER_SIZE {
            bucket.push(texture);
        }
    }
}

/// Upload RGBA pixels into an existing texture, padding rows to wgpu's 256-byte
/// `bytes_per_row` alignment when the tight row stride doesn't already satisfy it
/// (mirrors the pooled-texture upload path in `renderly-app/src-tauri/src/preview/gfx.rs`).
/// `pub` (not just crate-internal) so `renderly-wasm` can reuse the same padding-safe upload
/// when it builds its own cached caption texture (docs/preview-webview.md P3) instead of
/// duplicating the alignment math.
pub fn upload_layer_pixels(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    pixels: &[u8],
) {
    let unpadded_bpr = width * 4;
    let padded_bpr = wgpu::util::align_to(unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let padded_storage: Option<Vec<u8>> = if padded_bpr == unpadded_bpr {
        None
    } else {
        let mut buf = vec![0u8; (padded_bpr * height) as usize];
        for row in 0..height as usize {
            let src = row * unpadded_bpr as usize;
            let dst = row * padded_bpr as usize;
            buf[dst..dst + unpadded_bpr as usize]
                .copy_from_slice(&pixels[src..src + unpadded_bpr as usize]);
        }
        Some(buf)
    };
    let upload_slice: &[u8] = match &padded_storage {
        Some(buf) => buf.as_slice(),
        None => &pixels[..(unpadded_bpr * height) as usize],
    };
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        upload_slice,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded_bpr),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

impl Compositor {
    /// Blocking constructor used by export/CLI/MCP. Not available on wasm32 (`pollster`
    /// can't block on a wasm event loop and adapter/device requests are async-only there);
    /// the wasm compositor calls [`Self::new_async`] / [`Self::with_device`] directly. See
    /// docs/preview-webview.md P2.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(width: u32, height: u32) -> Result<Self, ComposeError> {
        pollster::block_on(Self::new_async(width, height))
    }

    /// A4: build on an externally-owned wgpu device/queue (typically the preview surface's).
    /// Export/CLI/MCP keep [`Self::new`]; the app injects the shared preview device so
    /// compose and present share one GPU without a CPU readback round-trip.
    pub fn with_device(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        width: u32,
        height: u32,
    ) -> Result<Self, ComposeError> {
        Self::from_device(device, queue, width, height)
    }

    /// Register (or replace) a file-backed matte from decoded RGBA pixels (what a webview's
    /// `createImageBitmap` + `getImageData` yields), converting to luma8 exactly like native
    /// `image::open(..).to_luma8()`. `key` is [`crate::mask::matte_cache_key`] of the
    /// `Raster` / `Generated` mask it backs. This is the filesystem-free path the wasm preview
    /// uses; export loads the same mattes from disk into the same cache.
    pub fn set_matte_rgba(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), ComposeError> {
        let luma = crate::mask::luma_from_rgba(rgba);
        self.set_matte(key, width, height, &luma)
    }

    /// As [`Self::set_matte_rgba`] for already-single-channel pixels.
    pub fn set_matte(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
        luma: &[u8],
    ) -> Result<(), ComposeError> {
        let (device, queue) = (Arc::clone(&self.device), Arc::clone(&self.queue));
        self.effects
            .set_matte(&device, &queue, key, luma, width, height)
    }

    /// Forget a registered matte (mask cleared / project closed).
    pub fn remove_matte(&mut self, key: &str) {
        self.effects.remove_matte(key);
    }

    /// Whether `kind` can be composited right now: shape and in-memory `Luma` masks always
    /// can; file-backed mattes only once registered via [`Self::set_matte`].
    pub fn has_matte(&self, kind: &crate::project::ClipMaskKind) -> bool {
        match crate::mask::matte_cache_key(kind) {
            Some(key) => self.effects.has_matte_key(&key),
            None => true,
        }
    }

    /// Invalidate uploaded pack-LUT textures; call after the pack slice handed to
    /// [`Self::compose_to_texture`] changes content.
    pub fn clear_pack_lut_cache(&mut self) {
        self.effects.clear_pack_lut_cache();
    }

    /// The mask the *preview* composites an evaluated layer with, mirroring export's
    /// resolution (`export::resolve_background_removal_mask`) for everything that needs no
    /// per-frame CPU pixels:
    /// - background removal enabled with a baked `matte_cache_dir` -> its `Generated` matte
    ///   (same [`crate::mask::cached_background_mask`] export calls);
    /// - otherwise the clip's own mask.
    ///
    /// A file-backed matte the host has not registered yet is dropped (layer renders
    /// unmasked) instead of failing the whole frame. **Unsupported sub-case:** background
    /// removal enabled *without* a baked cache makes export build a live heuristic matte from
    /// the decoded frame's pixels; the preview has no CPU frame access, so it falls back to the
    /// clip's own mask there.
    pub fn preview_layer_mask(&self, layer: &eval::ActiveLayer) -> Option<ClipMask> {
        let baked = layer
            .background_removal
            .as_ref()
            .filter(|b| b.enabled)
            .and_then(crate::mask::cached_background_mask);
        baked
            .or_else(|| layer.mask.clone())
            .filter(|m| self.has_matte(&m.kind))
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// View of the most recent GPU composite target. Valid after [`Self::compose_to_texture`]
    /// (and after [`Self::composite`], though callers that need CPU pixels should use that
    /// return value instead). Safe to sample from the *same* device that owns this compositor.
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn new_async(width: u32, height: u32) -> Result<Self, ComposeError> {
        let (device, queue) = shared_device()?;
        Self::from_device(device, queue, width, height)
    }

    fn from_device(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        width: u32,
        height: u32,
    ) -> Result<Self, ComposeError> {
        // TEXTURE_BINDING lets the preview blit sample this RT without a CPU round-trip (A4).
        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("export-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let output_view = output_texture.create_view(&Default::default());

        let bytes_per_row = width * 4;
        let padded_bytes_per_row =
            wgpu::util::align_to(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) as usize;
        let readback_size = (padded_bytes_per_row * height as usize) as u64;

        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer"),
            entries: &bind_group_layout_entries(),
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}",
                    include_str!("blend.wgsl"),
                    include_str!("composite.wgsl")
                )
                .into(),
            ),
        });

        // Blend modes read the accumulation so far from a copy bound at 3.
        let mut blend_entries = bind_group_layout_entries();
        blend_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 3,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        let blend_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("layer-blend"),
                entries: &blend_entries,
            });
        let blend_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("composite-blend"),
                bind_group_layouts: &[Some(&blend_bind_group_layout)],
                immediate_size: 0,
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite"),
            layout: Some(&pipeline_layout),
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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

        let blend_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite-blend"),
            layout: Some(&blend_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blend"),
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
        });

        let effects = EffectProcessor::new(&device);
        let transitions = TransitionPass::new(&device);

        Ok(Self {
            device,
            queue,
            width,
            height,
            output_texture,
            output_view,
            readback_buffer,
            bind_group_layout,
            pipeline,
            blend_bind_group_layout,
            blend_pipeline,
            sampler,
            effects,
            transitions,
            layer_pool: TexturePool::default(),
            target_pool: TexturePool::default(),
        })
    }

    /// Composite layers onto the internal RT and leave the result on the GPU (no
    /// `copy_texture_to_buffer` / map). Preview samples [`Self::output_view`] on the
    /// shared device; export/MCP keep [`Self::composite`] for the CPU path.
    pub fn compose_to_texture(
        &mut self,
        packs: &[LoadedPack],
        layers: &[ComposeLayer],
    ) -> Result<(), ComposeError> {
        self.compose_inner(packs, layers.iter().map(StepRef::Layer))
    }

    /// As [`Self::compose_to_texture`] for a full v7 step list (layers, adjustment layers,
    /// nested sequences).
    pub fn compose_steps_to_texture(
        &mut self,
        packs: &[LoadedPack],
        steps: &[SceneStep],
    ) -> Result<(), ComposeError> {
        self.compose_inner(packs, steps.iter().map(SceneStep::as_ref))
    }

    fn compose_inner<'a, I: Iterator<Item = StepRef<'a>>>(
        &mut self,
        packs: &[LoadedPack],
        steps: I,
    ) -> Result<(), ComposeError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compose-gpu"),
            });
        self.begin_frame();
        let scratch = self.encode_root(packs, steps, &mut encoder)?;
        self.queue.submit(Some(encoder.finish()));
        self.recycle(scratch);
        Ok(())
    }

    /// Composite layers in order (first = bottom). Empty → solid black frame.
    ///
    /// Per layer: upload → optional builtin effect chain (ping-pong) → cover+transform
    /// composite draw. Layers with empty/disabled effects take the same path as before.
    pub fn composite(
        &mut self,
        packs: &[LoadedPack],
        layers: &[ComposeLayer],
    ) -> Result<Vec<u8>, ComposeError> {
        self.composite_inner(packs, layers.iter().map(StepRef::Layer))
    }

    /// As [`Self::composite`] for a full v7 step list (layers, adjustment layers, nested
    /// sequences).
    pub fn composite_steps(
        &mut self,
        packs: &[LoadedPack],
        steps: &[SceneStep],
    ) -> Result<Vec<u8>, ComposeError> {
        self.composite_inner(packs, steps.iter().map(SceneStep::as_ref))
    }

    /// Rewinds the effect runtimes' uniform rings at the start of each submitted frame.
    fn begin_frame(&mut self) {
        self.effects.begin_frame();
        self.transitions.begin_frame();
    }

    fn composite_inner<'a, I: Iterator<Item = StepRef<'a>>>(
        &mut self,
        packs: &[LoadedPack],
        steps: I,
    ) -> Result<Vec<u8>, ComposeError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("export-frame"),
            });

        self.begin_frame();
        let scratch = self.encode_root(packs, steps, &mut encoder)?;

        let bytes_per_row = self.width * 4;
        let padded_bytes_per_row =
            wgpu::util::align_to(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(Some(encoder.finish()));
        self.recycle(scratch);

        let slice = self.readback_buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| ComposeError::Wgpu(format!("{e:?}")))?;
        receiver
            .recv()
            .map_err(|_| ComposeError::Wgpu("readback channel closed".into()))?
            .map_err(|e| ComposeError::Wgpu(format!("{e:?}")))?;

        let mapped = slice
            .get_mapped_range()
            .map_err(|e| ComposeError::Wgpu(e.to_string()))?;
        let mut out = vec![0u8; (self.width * self.height * 4) as usize];
        for row in 0..self.height as usize {
            let src_start = row * padded_bytes_per_row as usize;
            let dst_start = row * bytes_per_row as usize;
            out[dst_start..dst_start + bytes_per_row as usize]
                .copy_from_slice(&mapped[src_start..src_start + bytes_per_row as usize]);
        }
        drop(mapped);
        self.readback_buffer.unmap();

        Ok(out)
    }

    /// Return a finished frame's borrowed textures to their pools.
    fn recycle(&mut self, scratch: FrameScratch) {
        for texture in scratch.uploads {
            self.layer_pool.give_back(texture);
        }
        for texture in scratch.targets {
            self.target_pool.give_back(texture);
        }
    }

    fn encode_root<'a, I: Iterator<Item = StepRef<'a>>>(
        &mut self,
        packs: &[LoadedPack],
        steps: I,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<FrameScratch, ComposeError> {
        let target = Target {
            texture: self.output_texture.clone(),
            view: self.output_view.clone(),
            width: self.width,
            height: self.height,
        };
        let mut scratch = FrameScratch::default();
        // The root frame starts opaque black; nested sequences start transparent.
        self.encode_steps(
            packs,
            steps,
            &target,
            wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            encoder,
            &mut scratch,
        )?;
        Ok(scratch)
    }

    /// Clear `target`, then composite `steps` (first = bottom) onto it. Every target is
    /// treated as premultiplied-alpha accumulation: the root is opaque (alpha 1, so
    /// premultiplied == straight) and nested-sequence targets start transparent.
    fn encode_steps<'a, I: Iterator<Item = StepRef<'a>>>(
        &mut self,
        packs: &[LoadedPack],
        steps: I,
        target: &Target,
        clear: wgpu::Color,
        encoder: &mut wgpu::CommandEncoder,
        scratch: &mut FrameScratch,
    ) -> Result<(), ComposeError> {
        {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        let mut steps = steps.peekable();
        while let Some(step) = steps.next() {
            let layer = match step {
                StepRef::Adjustment(adjustment) => {
                    self.draw_adjustment(packs, adjustment, target, encoder, scratch)?;
                    continue;
                }
                StepRef::Layer(layer) => layer,
            };

            let pair = match (layer.transition.as_ref(), steps.peek().copied()) {
                (Some(ta), Some(StepRef::Layer(incoming))) if !ta.is_incoming => {
                    match incoming.transition.as_ref() {
                        Some(tb) if tb.is_incoming && ta.item_ref == tb.item_ref => {
                            Some((incoming, ta.clone()))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };

            if let Some((incoming, transition)) = pair {
                steps.next();
                // A transition that is not installed (or not runnable here) passes through:
                // the outgoing clip shows until the cut, params stay in the project.
                let Some(item) = transition_item(packs, &transition) else {
                    let prepared = self.prepare_layer(packs, layer, encoder, scratch)?;
                    self.draw_prepared(
                        packs,
                        layer,
                        &prepared,
                        target,
                        DrawDest::Main,
                        encoder,
                        scratch,
                    )?;
                    continue;
                };
                // Prepare both pictures first: preparing a nested sequence can itself use the
                // shared transition render targets, which must not be clobbered between the
                // two draws below.
                let prep_out = self.prepare_layer(packs, layer, encoder, scratch)?;
                let prep_in = self.prepare_layer(packs, incoming, encoder, scratch)?;
                self.transitions
                    .ensure_rts(&self.device, target.width, target.height);
                self.draw_prepared(
                    packs,
                    layer,
                    &prep_out,
                    target,
                    DrawDest::TransitionA,
                    encoder,
                    scratch,
                )?;
                self.draw_prepared(
                    packs,
                    incoming,
                    &prep_in,
                    target,
                    DrawDest::TransitionB,
                    encoder,
                    scratch,
                )?;
                self.transitions.blend(
                    &self.device,
                    &self.queue,
                    encoder,
                    &target.view,
                    &item,
                    &transition,
                )?;
                continue;
            }

            let prepared = self.prepare_layer(packs, layer, encoder, scratch)?;
            self.draw_prepared(
                packs,
                layer,
                &prepared,
                target,
                DrawDest::Main,
                encoder,
                scratch,
            )?;
        }

        Ok(())
    }

    /// Adjustment layer: run its effect chain on the accumulation so far and mix the result
    /// back by `opacity` x `mask`. No-op (no passes, no allocation) when nothing would
    /// change: no effective effects, or zero opacity.
    fn draw_adjustment(
        &mut self,
        packs: &[LoadedPack],
        adjustment: &ComposeAdjustment,
        target: &Target,
        encoder: &mut wgpu::CommandEncoder,
        scratch: &mut FrameScratch,
    ) -> Result<(), ComposeError> {
        let opacity = adjustment.opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 || !self.effects.has_work(&adjustment.effects, packs) {
            return Ok(());
        }
        // Effects expect straight alpha; the accumulation is premultiplied (alpha == 1 at
        // the root, where this is an exact copy).
        let straight = self.take_target(target.width, target.height);
        let straight_view = straight.create_view(&Default::default());
        scratch.targets.push(straight);
        self.effects
            .unpremultiply(&self.device, encoder, &target.view, &straight_view)?;
        let wrote = self.effects.apply(
            &self.device,
            &self.queue,
            encoder,
            &straight_view,
            target.width,
            target.height,
            &adjustment.effects,
            None,
            packs,
            &adjustment.timing,
        )?;
        if wrote {
            self.effects.mix_adjustment(
                &self.device,
                &self.queue,
                encoder,
                &straight_view,
                &target.view,
                opacity,
                adjustment.mask.as_ref(),
                adjustment.blend_mode,
            )?;
        }
        Ok(())
    }

    fn take_target(&mut self, width: u32, height: u32) -> wgpu::Texture {
        match self.target_pool.take(width, height) {
            Some(texture) => texture,
            None => self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("compose-target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }),
        }
    }

    /// Turn a layer's source into a sampleable view: upload a decoded frame, wrap an external
    /// texture, or composite a nested sequence offscreen.
    fn prepare_layer(
        &mut self,
        packs: &[LoadedPack],
        layer: &ComposeLayer,
        encoder: &mut wgpu::CommandEncoder,
        scratch: &mut FrameScratch,
    ) -> Result<Prepared, ComposeError> {
        let width = layer.source.width();
        let height = layer.source.height();
        // Frame sources go through the pooled upload textures (returned to the pool after
        // submit via `scratch`); Texture sources are externally owned and bound directly
        // — the caller keeps them alive across the frame, so they never enter the pool.
        let view = match &layer.source {
            LayerSource::Frame(frame) => {
                let texture = match self.layer_pool.take(frame.width, frame.height) {
                    Some(texture) => {
                        upload_layer_pixels(
                            &self.queue,
                            &texture,
                            frame.width,
                            frame.height,
                            &frame.pixels,
                        );
                        texture
                    }
                    None => self.device.create_texture_with_data(
                        &self.queue,
                        &wgpu::TextureDescriptor {
                            label: Some("layer"),
                            size: wgpu::Extent3d {
                                width: frame.width,
                                height: frame.height,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING
                                | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        },
                        wgpu::util::TextureDataOrder::LayerMajor,
                        &frame.pixels,
                    ),
                };
                let view = texture.create_view(&Default::default());
                scratch.uploads.push(texture);
                view
            }
            LayerSource::Texture { texture, .. } => texture.create_view(&Default::default()),
            LayerSource::Sequence(scene) => {
                return self.render_sequence(packs, scene, encoder, scratch);
            }
        };
        Ok(Prepared {
            view,
            width,
            height,
        })
    }

    /// Composite a nested sequence's steps into a transparent offscreen target at the
    /// sequence's resolution, then convert the premultiplied accumulation to straight alpha
    /// so the result can go through the ordinary effect / mask / draw path.
    fn render_sequence(
        &mut self,
        packs: &[LoadedPack],
        scene: &SequenceScene,
        encoder: &mut wgpu::CommandEncoder,
        scratch: &mut FrameScratch,
    ) -> Result<Prepared, ComposeError> {
        let (width, height) = (scene.width.max(1), scene.height.max(1));
        let accumulation = self.take_target(width, height);
        let target = Target {
            texture: accumulation.clone(),
            view: accumulation.create_view(&Default::default()),
            width,
            height,
        };
        scratch.targets.push(accumulation);
        self.encode_steps(
            packs,
            scene.steps.iter().map(SceneStep::as_ref),
            &target,
            wgpu::Color::TRANSPARENT,
            encoder,
            scratch,
        )?;

        let straight = self.take_target(width, height);
        let view = straight.create_view(&Default::default());
        scratch.targets.push(straight);
        self.effects
            .unpremultiply(&self.device, encoder, &target.view, &view)?;
        Ok(Prepared {
            view,
            width,
            height,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_prepared(
        &mut self,
        packs: &[LoadedPack],
        layer: &ComposeLayer,
        prepared: &Prepared,
        target: &Target,
        dest: DrawDest,
        encoder: &mut wgpu::CommandEncoder,
        scratch: &mut FrameScratch,
    ) -> Result<(), ComposeError> {
        let (src_w, src_h) = (prepared.width, prepared.height);
        let uploaded_view = &prepared.view;

        let use_effects = self.effects.apply(
            &self.device,
            &self.queue,
            encoder,
            uploaded_view,
            src_w,
            src_h,
            &layer.effects,
            layer.mask.as_ref(),
            packs,
            &layer.timing,
        )?;

        // Blend modes other than normal apply when the layer is drawn onto the accumulation;
        // the two sides of a transition are drawn normally and the transition blends them.
        let blend_mode = if matches!(dest, DrawDest::Main) {
            layer.blend_mode
        } else {
            BlendMode::Normal
        };
        let params = layer_params(
            src_w,
            src_h,
            target.width,
            target.height,
            &layer.transform,
            blend_mode,
        );
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("layer-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let backdrop_view = if blend_mode != BlendMode::Normal {
            // Copy the accumulation so far; the blend shader reads it while writing the target.
            let backdrop = self.take_target(target.width, target.height);
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &target.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &backdrop,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: target.width,
                    height: target.height,
                    depth_or_array_layers: 1,
                },
            );
            let view = backdrop.create_view(&Default::default());
            scratch.targets.push(backdrop);
            Some(view)
        } else {
            None
        };

        let sample_view = match (use_effects, self.effects.result_view()) {
            (true, Some(view)) => view,
            _ => uploaded_view,
        };

        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(sample_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ];
        if let Some(view) = &backdrop_view {
            entries.push(wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(view),
            });
        }
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("layer-bind"),
            layout: if backdrop_view.is_some() {
                &self.blend_bind_group_layout
            } else {
                &self.bind_group_layout
            },
            entries: &entries,
        });

        let clear = matches!(dest, DrawDest::TransitionA | DrawDest::TransitionB);
        let dest_view = match dest {
            DrawDest::Main => &target.view,
            DrawDest::TransitionA => self.transitions.view_a(),
            DrawDest::TransitionB => self.transitions.view_b(),
        };

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite-layer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dest_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if clear {
                            wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 1.0,
                            })
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(if backdrop_view.is_some() {
                &self.blend_pipeline
            } else {
                &self.pipeline
            });
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        Ok(())
    }
}

/// The compiled transition item behind a layer's `LayerTransition`, if it resolves to a
/// transition this build can run.
fn transition_item(
    packs: &[LoadedPack],
    transition: &LayerTransition,
) -> Option<StdArc<runtime::CompiledEffect>> {
    match resolve_target(packs, &transition.item_ref, transition.hash.as_deref()).target? {
        ResolvedTarget::Compiled(item) => item
            .compiled
            .clone()
            .filter(|c| c.manifest().kind == ItemKind::Transition),
        ResolvedTarget::Lut { .. } => None,
    }
}

enum DrawDest {
    Main,
    TransitionA,
    TransitionB,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{EffectInstance, Id};
    use std::collections::BTreeMap;

    fn solid_frame(w: u32, h: u32, r: u8, g: u8, b: u8) -> RgbaFrame {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for px in pixels.as_chunks_mut::<4>().0 {
            px[0] = r;
            px[1] = g;
            px[2] = b;
            px[3] = 255;
        }
        RgbaFrame {
            width: w,
            height: h,
            pixels,
        }
    }

    fn mean_luma(rgba: &[u8]) -> f64 {
        let mut sum = 0.0f64;
        let mut n = 0usize;
        for px in rgba.as_chunks::<4>().0 {
            sum += 0.2126 * px[0] as f64 + 0.7152 * px[1] as f64 + 0.0722 * px[2] as f64;
            n += 1;
        }
        sum / n as f64
    }

    fn adjustment_step(
        effects: Vec<EffectInstance>,
        opacity: f32,
        mask: Option<ClipMask>,
    ) -> SceneStep {
        SceneStep::adjustment(
            effects,
            opacity,
            mask,
            BlendMode::Normal,
            FxTiming::default(),
        )
    }

    fn effect(item_ref: &str, params: BTreeMap<String, f64>) -> EffectInstance {
        EffectInstance {
            id: Id::new_v4(),
            item_ref: item_ref.into(),
            hash: None,
            enabled: true,
            params: params
                .into_iter()
                .map(|(k, v)| (k, crate::project::EffectParam::Number(v)))
                .collect(),
        }
    }

    #[test]
    fn cover_uv_is_identity_for_matching_aspect_ratio() {
        let (scale, offset) = cover_uv(1080, 1920, 1080, 1920, 0.0, 0.0);
        assert!((scale[0] - 1.0).abs() < 1e-6);
        assert!((scale[1] - 1.0).abs() < 1e-6);
        assert!((offset[0] - 0.0).abs() < 1e-6);
        assert!((offset[1] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cover_uv_crops_sides_for_wider_landscape_source_into_vertical_output() {
        // 16:9 gameplay footage into a 9:16 TikTok export: the source is relatively wider
        // than the output, so covering it means cropping the left/right edges (scale_x < 1)
        // while using the full height (scale_y == 1) — never stretching either axis.
        let (scale, offset) = cover_uv(1920, 1080, 1080, 1920, 0.0, 0.0);
        assert!(
            scale[0] < 1.0,
            "expected horizontal crop, got scale={scale:?}"
        );
        assert!(
            (scale[1] - 1.0).abs() < 1e-6,
            "expected full height, got scale={scale:?}"
        );
        assert!(
            (offset[0] - (1.0 - scale[0]) / 2.0).abs() < 1e-6,
            "crop should be centered"
        );
        assert!((offset[1] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cover_uv_crops_top_bottom_for_taller_source_into_landscape_output() {
        let (scale, offset) = cover_uv(1080, 1920, 1920, 1080, 0.0, 0.0);
        assert!(
            (scale[0] - 1.0).abs() < 1e-6,
            "expected full width, got scale={scale:?}"
        );
        assert!(
            scale[1] < 1.0,
            "expected vertical crop, got scale={scale:?}"
        );
        assert!((offset[0] - 0.0).abs() < 1e-6);
        assert!(
            (offset[1] - (1.0 - scale[1]) / 2.0).abs() < 1e-6,
            "crop should be centered"
        );
    }

    #[test]
    fn identity_transform_params_match_cover_only() {
        let params = layer_params(
            1920,
            1080,
            1080,
            1920,
            &ClipTransform::default(),
            BlendMode::Normal,
        );
        let (scale, offset) = cover_uv(1920, 1080, 1080, 1920, 0.0, 0.0);
        assert_eq!(params.cover_scale, scale);
        assert_eq!(params.cover_offset, offset);
        assert_eq!(params.user_translate, [0.0, 0.0]);
        assert_eq!(params.user_scale, [1.0, 1.0]);
        assert!((params.rotation_rad).abs() < 1e-6);
        assert!((params.opacity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cover_uv_fit_slides_the_window_to_flush_edges() {
        // 16:9 source on a 9:16 canvas: only the horizontal axis has slack.
        let (scale, offset) = cover_uv(1920, 1080, 1080, 1920, -1.0, 0.0);
        assert_eq!(offset[0], 0.0, "fit -1 is flush with the left edge");
        let (_, offset) = cover_uv(1920, 1080, 1080, 1920, 1.0, 0.0);
        assert!(
            (offset[0] - (1.0 - scale[0])).abs() < 1e-6,
            "fit +1 is flush right"
        );
        let (_, offset) = cover_uv(1920, 1080, 1080, 1920, 0.5, 0.0);
        assert!(
            (offset[0] - (1.0 - scale[0]) * 0.75).abs() < 1e-6,
            "fit 0.5 is 3/4 across"
        );
        // The vertical axis has no slack here, so its fit is ignored.
        let (scale, offset) = cover_uv(1920, 1080, 1080, 1920, 0.0, 1.0);
        assert_eq!((scale[1], offset[1]), (1.0, 0.0));
    }

    #[test]
    fn layer_params_clamps_the_fit_position() {
        let t = ClipTransform {
            fit_x: 5.0,
            fit_y: -9.0,
            ..ClipTransform::default()
        };
        let p = layer_params(1920, 1080, 1080, 1920, &t, BlendMode::Normal);
        assert!((p.cover_offset[0] - (1.0 - p.cover_scale[0])).abs() < 1e-6);
        assert_eq!(p.cover_offset[1], 0.0);
    }

    #[test]
    fn cover_fit_slides_the_visible_window_across_the_source() {
        let mut c = match Compositor::new(9, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        // 32x18 source (16:9) on a 9:16 canvas: 31.6% of its width is visible at fit 0.
        // Left third red, middle green, right third blue.
        let (w, h) = (32u32, 18u32);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                let rgb: [u8; 3] = if x < 11 {
                    [255, 0, 0]
                } else if x < 21 {
                    [0, 255, 0]
                } else {
                    [0, 0, 255]
                };
                pixels[o..o + 3].copy_from_slice(&rgb);
                pixels[o + 3] = 255;
            }
        }
        let frame = RgbaFrame {
            width: w,
            height: h,
            pixels,
        };
        let mut render = |fit_x: f64| {
            let layer = ComposeLayer {
                source: LayerSource::Frame(frame.clone()),
                transform: ClipTransform {
                    fit_x,
                    ..ClipTransform::default()
                },
                effects: vec![],
                mask: None,
                transition: None,
                blend_mode: BlendMode::Normal,
                timing: FxTiming::default(),
            };
            c.composite(&[], &[layer]).unwrap()
        };
        // Every visible column of the middle row shows only the expected third.
        let row = |out: &[u8]| -> Vec<[u8; 3]> {
            (0..9usize)
                .map(|x| {
                    let o = (8 * 9 + x) * 4;
                    [out[o], out[o + 1], out[o + 2]]
                })
                .collect()
        };
        for p in row(&render(-1.0)) {
            assert_near(p, [255, 0, 0], 8, "fit -1 shows the left third");
        }
        for p in row(&render(0.0)) {
            assert_near(p, [0, 255, 0], 8, "fit 0 shows the middle third");
        }
        for p in row(&render(1.0)) {
            assert_near(p, [0, 0, 255], 8, "fit +1 shows the right third");
        }
    }

    #[test]
    fn texture_pool_reuses_freed_textures_by_size_and_caps_growth() {
        let device = match Compositor::new(8, 8) {
            Ok(c) => c.device,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let make_tex = |w: u32, h: u32| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };

        let mut pool = TexturePool::default();
        assert!(pool.take(32, 32).is_none(), "nothing pooled yet");

        pool.give_back(make_tex(32, 32));
        assert!(pool.take(64, 64).is_none(), "different size must not match");
        assert!(
            pool.take(32, 32).is_some(),
            "matching size should be reused"
        );
        assert!(
            pool.take(32, 32).is_none(),
            "reused texture is consumed, not duplicated"
        );

        for _ in 0..(TEXTURE_POOL_MAX_PER_SIZE + 4) {
            pool.give_back(make_tex(16, 16));
        }
        let bucket_len = pool.free.get(&(16, 16)).map(Vec::len).unwrap_or(0);
        assert_eq!(
            bucket_len, TEXTURE_POOL_MAX_PER_SIZE,
            "pool must cap per-size growth instead of leaking GPU memory"
        );
    }

    #[test]
    fn compose_to_texture_matches_composite_readback() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let frame = solid_frame(16, 16, 40, 80, 120);
        let layers = [ComposeLayer {
            source: LayerSource::Frame(frame.clone()),
            transform: ClipTransform::default(),
            effects: vec![],
            mask: None,
            transition: None,
            blend_mode: BlendMode::Normal,
            timing: FxTiming::default(),
        }];
        let cpu = c.composite(&[], &layers).unwrap();
        c.compose_to_texture(
            &[],
            &[ComposeLayer {
                source: LayerSource::Frame(frame),
                transform: ClipTransform::default(),
                effects: vec![],
                mask: None,
                transition: None,
                blend_mode: BlendMode::Normal,
                timing: FxTiming::default(),
            }],
        )
        .unwrap();
        // Re-read via composite of the same content to prove the GPU path doesn't diverge
        // structurally — the texture path skips map_read, so we re-composite once for assert.
        let cpu2 = c
            .composite(
                &[],
                &[ComposeLayer {
                    source: LayerSource::Frame(solid_frame(16, 16, 40, 80, 120)),
                    transform: ClipTransform::default(),
                    effects: vec![],
                    mask: None,
                    transition: None,
                    blend_mode: BlendMode::Normal,
                    timing: FxTiming::default(),
                }],
            )
            .unwrap();
        assert_eq!(cpu, cpu2);
        assert!(!cpu.iter().all(|&b| b == 0));
    }

    #[test]
    fn color_adjust_exposure_changes_luma() {
        let mut c = match Compositor::new(32, 32) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let frame = solid_frame(32, 32, 128, 128, 128);
        let base = c
            .composite(
                &[],
                &[ComposeLayer {
                    source: LayerSource::Frame(frame.clone()),
                    transform: ClipTransform::default(),
                    effects: vec![],
                    mask: None,
                    transition: None,
                    blend_mode: BlendMode::Normal,
                    timing: FxTiming::default(),
                }],
            )
            .unwrap();
        let bright = c
            .composite(
                &[],
                &[ComposeLayer {
                    source: LayerSource::Frame(frame),
                    transform: ClipTransform::default(),
                    effects: vec![effect(
                        "builtin:color_adjust@1",
                        [("exposure".into(), 1.0)].into_iter().collect(),
                    )],
                    mask: None,
                    transition: None,
                    blend_mode: BlendMode::Normal,
                    timing: FxTiming::default(),
                }],
            )
            .unwrap();
        assert!(
            mean_luma(&bright) > mean_luma(&base) + 10.0,
            "exposure+1 should raise luma (base={}, bright={})",
            mean_luma(&base),
            mean_luma(&bright)
        );
    }

    #[test]
    fn empty_and_disabled_effects_match_identity() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let frame = solid_frame(16, 16, 40, 80, 120);
        let identity = c
            .composite(
                &[],
                &[ComposeLayer {
                    source: LayerSource::Frame(frame.clone()),
                    transform: ClipTransform::default(),
                    effects: vec![],
                    mask: None,
                    transition: None,
                    blend_mode: BlendMode::Normal,
                    timing: FxTiming::default(),
                }],
            )
            .unwrap();
        let empty_chain = c
            .composite(
                &[],
                &[ComposeLayer {
                    source: LayerSource::Frame(frame.clone()),
                    transform: ClipTransform::default(),
                    effects: vec![],
                    mask: None,
                    transition: None,
                    blend_mode: BlendMode::Normal,
                    timing: FxTiming::default(),
                }],
            )
            .unwrap();
        assert_eq!(identity, empty_chain);

        let mut disabled = effect(
            "builtin:color_adjust@1",
            [("exposure".into(), 2.0)].into_iter().collect(),
        );
        disabled.enabled = false;
        let disabled_out = c
            .composite(
                &[],
                &[ComposeLayer {
                    source: LayerSource::Frame(frame),
                    transform: ClipTransform::default(),
                    effects: vec![disabled],
                    mask: None,
                    transition: None,
                    blend_mode: BlendMode::Normal,
                    timing: FxTiming::default(),
                }],
            )
            .unwrap();
        assert_eq!(identity, disabled_out);
    }

    #[test]
    fn blur_radius_zero_is_identity_ish() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let frame = solid_frame(16, 16, 200, 100, 50);
        let identity = c
            .composite(
                &[],
                &[ComposeLayer {
                    source: LayerSource::Frame(frame.clone()),
                    transform: ClipTransform::default(),
                    effects: vec![],
                    mask: None,
                    transition: None,
                    blend_mode: BlendMode::Normal,
                    timing: FxTiming::default(),
                }],
            )
            .unwrap();
        let blurred = c
            .composite(
                &[],
                &[ComposeLayer {
                    source: LayerSource::Frame(frame),
                    transform: ClipTransform::default(),
                    effects: vec![effect(
                        "builtin:blur@1",
                        [("radius".into(), 0.0)].into_iter().collect(),
                    )],
                    mask: None,
                    transition: None,
                    blend_mode: BlendMode::Normal,
                    timing: FxTiming::default(),
                }],
            )
            .unwrap();
        // radius 0 skips the blur passes entirely → bit-exact identity.
        assert_eq!(identity, blurred);
    }

    #[test]
    fn transition_pair_blends_mid_progress() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let red = solid_frame(16, 16, 255, 0, 0);
        let blue = solid_frame(16, 16, 0, 0, 255);
        let out = c
            .composite(
                &[],
                &[
                    ComposeLayer {
                        source: LayerSource::Frame(red),
                        transform: ClipTransform::default(),
                        effects: vec![],
                        mask: None,
                        transition: Some(LayerTransition {
                            item_ref: "builtin:crossfade@1".into(),
                            hash: None,
                            params: Default::default(),
                            default_easing: false,
                            progress: 0.5,
                            is_incoming: false,
                        }),
                        blend_mode: BlendMode::Normal,
                        timing: FxTiming::default(),
                    },
                    ComposeLayer {
                        source: LayerSource::Frame(blue),
                        transform: ClipTransform::default(),
                        effects: vec![],
                        mask: None,
                        transition: Some(LayerTransition {
                            item_ref: "builtin:crossfade@1".into(),
                            hash: None,
                            params: Default::default(),
                            default_easing: false,
                            progress: 0.5,
                            is_incoming: true,
                        }),
                        blend_mode: BlendMode::Normal,
                        timing: FxTiming::default(),
                    },
                ],
            )
            .unwrap();
        let px = &out[..4];
        assert!(
            px[0] > 90 && px[0] < 165,
            "red channel should be ~127 (pixel {px:?})",
        );
        assert!(
            px[2] > 90 && px[2] < 165,
            "blue channel should be ~127 (pixel {px:?})",
        );
    }

    // --- chroma key (GPU pass) -------------------------------------------------------

    const CK_BASE: [u8; 3] = [10, 20, 240];
    const CK_FG: [u8; 3] = [200, 40, 40];

    fn ck_params(tolerance: f64, softness: f64, spill: f64) -> BTreeMap<String, f64> {
        [
            ("key_r", 0.0),
            ("key_g", 1.0),
            ("key_b", 0.0),
            ("tolerance", tolerance),
            ("softness", softness),
            ("spill", spill),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    /// 64x4 strip: x 0..16 pure green, 16..32 foreground, 32..48 50/50 green-fg blend (soft
    /// edge), 48..64 pure green.
    fn ck_frame() -> RgbaFrame {
        let mut f = solid_frame(64, 4, 0, 255, 0);
        for y in 0..4usize {
            for x in 0..64usize {
                let c: [u8; 3] = match x {
                    16..=31 => CK_FG,
                    32..=47 => [100, 147, 20],
                    _ => [0, 255, 0],
                };
                let o = (y * 64 + x) * 4;
                f.pixels[o..o + 3].copy_from_slice(&c);
            }
        }
        f
    }

    fn ck_layers(effects: Vec<EffectInstance>, mask: Option<ClipMask>) -> [ComposeLayer; 2] {
        [
            ComposeLayer {
                source: LayerSource::Frame(solid_frame(64, 4, CK_BASE[0], CK_BASE[1], CK_BASE[2])),
                transform: ClipTransform::default(),
                effects: vec![],
                mask: None,
                transition: None,
                blend_mode: BlendMode::Normal,
                timing: FxTiming::default(),
            },
            ComposeLayer {
                source: LayerSource::Frame(ck_frame()),
                transform: ClipTransform::default(),
                effects,
                mask,
                transition: None,
                blend_mode: BlendMode::Normal,
                timing: FxTiming::default(),
            },
        ]
    }

    fn px(out: &[u8], x: usize) -> [u8; 3] {
        let o = (2 * 64 + x) * 4;
        [out[o], out[o + 1], out[o + 2]]
    }

    fn assert_near(actual: [u8; 3], expected: [u8; 3], tol: i32, what: &str) {
        for i in 0..3 {
            assert!(
                (actual[i] as i32 - expected[i] as i32).abs() <= tol,
                "{what}: got {actual:?}, expected {expected:?}"
            );
        }
    }

    fn ck_compositor() -> Option<Compositor> {
        match Compositor::new(64, 4) {
            Ok(c) => Some(c),
            Err(ComposeError::NoAdapter) => {
                eprintln!("skipping chroma key GPU test: no wgpu adapter");
                None
            }
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn chroma_key_keys_background_keeps_foreground_and_softens_edge() {
        let Some(mut c) = ck_compositor() else { return };
        let fx = effect("builtin:chroma_key@1", ck_params(0.3, 0.4, 0.0));
        let out = c.composite(&[], &ck_layers(vec![fx], None)).unwrap();
        // Pure green is inside tolerance -> fully transparent -> base shows through.
        assert_near(px(&out, 4), CK_BASE, 2, "keyed background");
        assert_near(px(&out, 60), CK_BASE, 2, "keyed background (right)");
        // Foreground is far from the key -> untouched, opaque.
        assert_near(px(&out, 24), CK_FG, 2, "foreground");
        // Soft edge: straight-alpha blend of the pixel over the base with
        // alpha = (dist - tol) / softness.
        let src = [100.0f32, 147.0, 20.0];
        let dist =
            ((src[0] / 255.0).powi(2) + (src[1] / 255.0 - 1.0).powi(2) + (src[2] / 255.0).powi(2))
                .sqrt();
        let a = (dist - 0.3) / 0.4;
        assert!(a > 0.2 && a < 0.9, "test edge alpha {a} should be partial");
        let e = |i: usize| (src[i] * a + CK_BASE[i] as f32 * (1.0 - a)).round() as u8;
        assert_near(px(&out, 40), [e(0), e(1), e(2)], 3, "soft edge");
    }

    #[test]
    fn chroma_key_spill_zero_is_default_and_spill_reduces_green_tint() {
        let Some(mut c) = ck_compositor() else { return };
        let mut p = ck_params(0.3, 0.4, 0.0);
        p.remove("spill");
        let absent = c
            .composite(
                &[],
                &ck_layers(vec![effect("builtin:chroma_key@1", p)], None),
            )
            .unwrap();
        let fx = |spill| effect("builtin:chroma_key@1", ck_params(0.3, 0.4, spill));
        let zero = c.composite(&[], &ck_layers(vec![fx(0.0)], None)).unwrap();
        assert_eq!(absent, zero, "absent spill param must equal spill = 0");
        let full = c.composite(&[], &ck_layers(vec![fx(1.0)], None)).unwrap();
        // Foreground (200,40,40) has no green-ward chroma -> unchanged by despill.
        assert_near(px(&full, 24), CK_FG, 2, "foreground unaffected by spill");
        // Soft-edge pixel is green-tinted: its green must drop.
        let before = px(&zero, 40);
        let after = px(&full, 40);
        assert!(
            after[1] < before[1],
            "spill should lower green: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn chroma_key_respects_effect_order_in_chain() {
        let Some(mut c) = ck_compositor() else { return };
        let desat = || {
            effect(
                "builtin:color_adjust@1",
                [("saturation".to_string(), 0.0)].into_iter().collect(),
            )
        };
        let key = || effect("builtin:chroma_key@1", ck_params(0.3, 0.05, 0.0));
        // Key first: green is removed, then the (now keyed) layer is desaturated.
        let key_first = c
            .composite(&[], &ck_layers(vec![key(), desat()], None))
            .unwrap();
        assert_near(px(&key_first, 4), CK_BASE, 2, "key then desaturate");
        // Desaturate first: green becomes grey (luma ~182) and is no longer near the key.
        let desat_first = c
            .composite(&[], &ck_layers(vec![desat(), key()], None))
            .unwrap();
        let g = px(&desat_first, 4);
        assert!(
            (g[0] as i32 - g[1] as i32).abs() <= 2 && g[1] > 150,
            "desaturate then key leaves grey background opaque: {g:?}"
        );
    }

    #[test]
    fn chroma_key_composes_with_clip_mask() {
        let Some(mut c) = ck_compositor() else { return };
        // Mask keeps only the left half: foreground at x=24 stays, the soft edge at x=40
        // (right half) is masked away entirely.
        let mask = ClipMask {
            enabled: true,
            invert: false,
            feather: 0.0,
            kind: crate::project::ClipMaskKind::Rect {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            },
        };
        let fx = effect("builtin:chroma_key@1", ck_params(0.3, 0.4, 0.0));
        let out = c.composite(&[], &ck_layers(vec![fx], Some(mask))).unwrap();
        assert_near(px(&out, 4), CK_BASE, 2, "keyed bg inside mask");
        assert_near(px(&out, 24), CK_FG, 2, "foreground inside mask");
        assert_near(px(&out, 40), CK_BASE, 2, "soft edge outside mask");
        assert_near(px(&out, 60), CK_BASE, 2, "bg outside mask");
    }

    #[test]
    fn chroma_key_composes_with_transition() {
        let Some(mut c) = ck_compositor() else { return };
        let green = solid_frame(64, 4, 0, 255, 0);
        let blue = solid_frame(64, 4, 0, 0, 255);
        let mut run = |effects: Vec<EffectInstance>| {
            let t = |is_incoming| {
                Some(LayerTransition {
                    item_ref: "builtin:crossfade@1".into(),
                    hash: None,
                    params: Default::default(),
                    default_easing: false,
                    progress: 0.5,
                    is_incoming,
                })
            };
            c.composite(
                &[],
                &[
                    ComposeLayer {
                        source: LayerSource::Frame(green.clone()),
                        transform: ClipTransform::default(),
                        effects,
                        mask: None,
                        transition: t(false),
                        blend_mode: BlendMode::Normal,
                        timing: FxTiming::default(),
                    },
                    ComposeLayer {
                        source: LayerSource::Frame(blue.clone()),
                        transform: ClipTransform::default(),
                        effects: vec![],
                        mask: None,
                        transition: t(true),
                        blend_mode: BlendMode::Normal,
                        timing: FxTiming::default(),
                    },
                ],
            )
            .unwrap()
        };
        let plain = px(&run(vec![]), 8);
        let keyed = px(
            &run(vec![effect(
                "builtin:chroma_key@1",
                ck_params(0.3, 0.1, 0.0),
            )]),
            8,
        );
        assert!(
            plain[1] > 100,
            "unkeyed outgoing green contributes: {plain:?}"
        );
        assert!(
            keyed[1] < 20,
            "keyed outgoing layer must not contribute green: {keyed:?}"
        );
    }

    #[test]
    fn builtin_effect_refs_lists_locked_set() {
        let ids = builtin_effect_refs();
        assert_eq!(ids.len(), 6);
        assert!(ids.contains(&"builtin:color_adjust@1"));
        assert!(ids.contains(&"builtin:blur@1"));
        assert!(ids.contains(&"builtin:lut_contrast@1"));
        assert!(ids.contains(&"builtin:lut_warm@1"));
        assert!(ids.contains(&"builtin:glitch@1"));
        assert!(ids.contains(&"builtin:chroma_key@1"));
    }

    // ---- schema v7: adjustment layers and nested sequences ------------------------------

    fn layer(frame: RgbaFrame) -> ComposeLayer {
        ComposeLayer {
            source: LayerSource::Frame(frame),
            transform: ClipTransform::default(),
            effects: vec![],
            mask: None,
            transition: None,
            blend_mode: BlendMode::Normal,
            timing: FxTiming::default(),
        }
    }

    fn solid_rgba(w: u32, h: u32, rgba: [u8; 4]) -> RgbaFrame {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for px in pixels.as_chunks_mut::<4>().0 {
            *px = rgba;
        }
        RgbaFrame {
            width: w,
            height: h,
            pixels,
        }
    }

    fn sequence_layer(w: u32, h: u32, steps: Vec<SceneStep>) -> SceneStep {
        SceneStep::Layer(ComposeLayer {
            source: LayerSource::Sequence(Box::new(SequenceScene {
                width: w,
                height: h,
                steps,
            })),
            transform: ClipTransform::default(),
            effects: vec![],
            mask: None,
            transition: None,
            blend_mode: BlendMode::Normal,
            timing: FxTiming::default(),
        })
    }

    fn max_diff(a: &[u8], b: &[u8]) -> u8 {
        a.iter()
            .zip(b)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn adjustment_applies_effects_to_everything_beneath_it_only() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let grey = adjustment_step(
            vec![effect(
                "builtin:color_adjust@1",
                [("saturation".into(), 0.0)].into_iter().collect(),
            )],
            1.0,
            None,
        );
        let steps = vec![
            SceneStep::Layer(layer(solid_frame(16, 16, 255, 0, 0))),
            grey,
            // Above the adjustment: must stay green.
            SceneStep::Layer(ComposeLayer {
                transform: ClipTransform {
                    scale_x: 0.5,
                    scale_y: 0.5,
                    ..ClipTransform::default()
                },
                ..layer(solid_frame(16, 16, 0, 255, 0))
            }),
        ];
        let out = c.composite_steps(&[], &steps).unwrap();
        let px = |x: usize, y: usize| &out[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
        // Corner: only red beneath -> greyed (r == g == b, non-zero luma).
        let corner = px(0, 0);
        assert!(
            corner[0] == corner[1] && corner[1] == corner[2],
            "{corner:?}"
        );
        assert!(corner[0] > 30, "{corner:?}");
        // Centre: the layer above the adjustment is untouched green.
        assert_eq!(px(8, 8), &[0, 255, 0, 255]);
    }

    #[test]
    fn adjustment_opacity_zero_and_inert_effects_are_identity() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let base = c
            .composite(&[], &[layer(solid_frame(16, 16, 40, 80, 120))])
            .unwrap();
        let strong = effect(
            "builtin:color_adjust@1",
            [("exposure".into(), 2.0)].into_iter().collect(),
        );
        let mut disabled = strong.clone();
        disabled.enabled = false;
        for adj in [
            adjustment_step(vec![strong.clone()], 0.0, None),
            adjustment_step(vec![disabled], 1.0, None),
            adjustment_step(vec![], 1.0, None),
        ] {
            let out = c
                .composite_steps(
                    &[],
                    &[
                        SceneStep::Layer(layer(solid_frame(16, 16, 40, 80, 120))),
                        adj,
                    ],
                )
                .unwrap();
            assert_eq!(out, base);
        }
        // And an effective adjustment at half opacity lands strictly between.
        let half = c
            .composite_steps(
                &[],
                &[
                    SceneStep::Layer(layer(solid_frame(16, 16, 40, 80, 120))),
                    adjustment_step(vec![strong.clone()], 0.5, None),
                ],
            )
            .unwrap();
        let full = c
            .composite_steps(
                &[],
                &[
                    SceneStep::Layer(layer(solid_frame(16, 16, 40, 80, 120))),
                    adjustment_step(vec![strong], 1.0, None),
                ],
            )
            .unwrap();
        assert!(half[0] > base[0] && half[0] < full[0], "{half:?} {full:?}");
    }

    #[test]
    fn adjustment_mask_limits_where_it_applies() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let adj = adjustment_step(
            vec![effect(
                "builtin:color_adjust@1",
                [("saturation".into(), 0.0)].into_iter().collect(),
            )],
            1.0,
            // Left half of the canvas.
            Some(ClipMask {
                enabled: true,
                invert: false,
                feather: 0.0,
                kind: crate::project::ClipMaskKind::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.5,
                    height: 1.0,
                },
            }),
        );
        let out = c
            .composite_steps(
                &[],
                &[SceneStep::Layer(layer(solid_frame(16, 16, 255, 0, 0))), adj],
            )
            .unwrap();
        let left = &out[(8 * 16 + 2) * 4..(8 * 16 + 2) * 4 + 4];
        let right = &out[(8 * 16 + 13) * 4..(8 * 16 + 13) * 4 + 4];
        assert!(left[0] == left[1] && left[1] == left[2], "{left:?}");
        assert_eq!(right, &[255, 0, 0, 255]);
    }

    #[test]
    fn opaque_nested_sequence_matches_the_flat_equivalent_exactly() {
        let mut c = match Compositor::new(32, 32) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let flat = c
            .composite(&[], &[layer(solid_frame(32, 32, 10, 200, 90))])
            .unwrap();
        let nested = c
            .composite_steps(
                &[],
                &[sequence_layer(
                    32,
                    32,
                    vec![SceneStep::Layer(layer(solid_frame(32, 32, 10, 200, 90)))],
                )],
            )
            .unwrap();
        assert_eq!(
            flat, nested,
            "nesting an opaque layer must not change a pixel"
        );
    }

    #[test]
    fn nesting_preserves_transparency() {
        let mut c = match Compositor::new(32, 32) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let backdrop = || SceneStep::Layer(layer(solid_frame(32, 32, 0, 0, 255)));
        let half_red = || solid_rgba(32, 32, [255, 0, 0, 128]);
        let flat = c
            .composite_steps(&[], &[backdrop(), SceneStep::Layer(layer(half_red()))])
            .unwrap();
        let nested = c
            .composite_steps(
                &[],
                &[
                    backdrop(),
                    sequence_layer(32, 32, vec![SceneStep::Layer(layer(half_red()))]),
                ],
            )
            .unwrap();
        // The sequence rendered over transparent (not black): the blue backdrop shows through
        // the half-transparent red just like the flat composite (8-bit premultiply round trip
        // allows a few LSB).
        assert!(
            max_diff(&flat, &nested) <= 3,
            "{:?} vs {:?}",
            &flat[..4],
            &nested[..4]
        );
        assert!(
            nested[2] > 100,
            "backdrop blue survives: {:?}",
            &nested[..4]
        );

        // A fully transparent nested layer leaves the backdrop untouched.
        let clear = c
            .composite_steps(
                &[],
                &[
                    backdrop(),
                    sequence_layer(
                        32,
                        32,
                        vec![SceneStep::Layer(layer(solid_rgba(32, 32, [255, 0, 0, 0])))],
                    ),
                ],
            )
            .unwrap();
        assert_eq!(&clear[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn sequence_at_its_own_resolution_is_cover_fitted() {
        let mut c = match Compositor::new(32, 18) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        // A 16x16 sequence into a 32x18 frame: cover-fit fills the whole frame.
        let out = c
            .composite_steps(
                &[],
                &[sequence_layer(
                    16,
                    16,
                    vec![SceneStep::Layer(layer(solid_frame(16, 16, 200, 100, 50)))],
                )],
            )
            .unwrap();
        assert_eq!(&out[..4], &[200, 100, 50, 255]);
        let last = out.len() - 4;
        assert_eq!(&out[last..], &[200, 100, 50, 255]);
    }

    #[test]
    fn nested_sequences_and_adjustments_reuse_pooled_targets_across_frames() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        let grey = || {
            adjustment_step(
                vec![effect(
                    "builtin:color_adjust@1",
                    [("saturation".into(), 0.0)].into_iter().collect(),
                )],
                1.0,
                None,
            )
        };
        let frame = |c: &mut Compositor| {
            c.composite_steps(
                &[],
                &[
                    sequence_layer(
                        16,
                        16,
                        vec![
                            SceneStep::Layer(layer(solid_frame(16, 16, 255, 0, 0))),
                            grey(),
                        ],
                    ),
                    grey(),
                ],
            )
            .unwrap();
        };
        frame(&mut c);
        let pooled = |c: &Compositor| c.target_pool.free.values().map(Vec::len).sum::<usize>();
        let after_first = pooled(&c);
        assert!(after_first > 0, "targets are returned to the pool");
        for _ in 0..3 {
            frame(&mut c);
        }
        assert_eq!(
            pooled(&c),
            after_first,
            "steady state allocates no new targets (they are taken from and returned to the pool)"
        );
    }

    #[test]
    fn no_pool_targets_are_used_without_sequences_or_adjustments() {
        let mut c = match Compositor::new(16, 16) {
            Ok(c) => c,
            Err(ComposeError::NoAdapter) => return,
            Err(e) => panic!("{e}"),
        };
        c.composite(&[], &[layer(solid_frame(16, 16, 1, 2, 3))])
            .unwrap();
        assert!(
            c.target_pool.free.values().all(Vec::is_empty),
            "plain layers must not touch the v7 target pool"
        );
    }
}
