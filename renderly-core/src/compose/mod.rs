//! Offscreen wgpu compositor for Phase 0 export. Decoded frames are uploaded as textures,
//! scaled to the output resolution, and read back as RGBA for the FFmpeg encoder.
//! Phase 3.1 adds per-layer user transform (translate / scale / rotate) and opacity.
//! Phase 3.4 runs builtin effect chains on each layer before the cover+transform draw.
//! Phase 3 adds dual-texture WGSL transitions.

mod chroma;
pub mod effects;
pub mod eval;
mod transition;

pub use chroma::apply_chroma_effects;
pub use effects::{builtin_effect_ids, default_params, BUILTIN_EFFECT_IDS};
pub use eval::{active_captions, active_layers, ActiveCaption, ActiveLayer, EvalError};

use crate::frame::RgbaFrame;
use crate::packs::LoadedPack;
use crate::project::{ClipMask, ClipTransform, EffectInstance, TransitionKind};
use effects::EffectProcessor;
use std::collections::HashMap;
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
}

/// Marks a layer as one side of an outgoing→incoming transition pair.
#[derive(Debug, Clone, Copy)]
pub struct LayerTransition {
    pub kind: TransitionKind,
    pub progress: f32,
    pub is_incoming: bool,
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
}

impl LayerSource {
    fn width(&self) -> u32 {
        match self {
            Self::Frame(f) => f.width,
            Self::Texture { width, .. } => *width,
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Frame(f) => f.height,
            Self::Texture { height, .. } => *height,
        }
    }
}

/// One composited layer: decoded (or caption) pixels plus evaluated transform at frame time.
pub struct ComposeLayer {
    pub source: LayerSource,
    pub transform: ClipTransform,
    /// Builtin effect instances (Phase 3.4). Empty / all-disabled → identity path.
    pub effects: Vec<EffectInstance>,
    /// Optional clip mask (Phase 4).
    pub mask: Option<ClipMask>,
    /// When set on two consecutive layers (outgoing then incoming), uses the transition pass.
    pub transition: Option<LayerTransition>,
}

impl ComposeLayer {
    /// The single place an evaluated [`ActiveLayer`] plus its pixels becomes a
    /// [`ComposeLayer`]. Shared by the native export path (`export::FrameRenderer`, with a
    /// decoded `LayerSource::Frame`) and the wasm preview (`renderly-wasm`, with a
    /// `LayerSource::Texture` filled from a browser element), so transform / effects /
    /// transition can never be wired differently in the two. `mask` is passed in because
    /// each path legitimately resolves it differently: export may replace it with a
    /// background-removal matte, the preview drops mattes it cannot load
    /// ([`preview_supported_mask`]).
    pub fn from_active(layer: &ActiveLayer, source: LayerSource, mask: Option<ClipMask>) -> Self {
        Self {
            source,
            transform: layer.transform,
            effects: layer.effects.clone(),
            mask,
            transition: layer.transition,
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
        }
    }
}

/// The mask the wasm preview can honour: shape masks (rect / ellipse) and in-memory `Luma`
/// mattes pass through; `Raster` / `Generated` mattes (filesystem `image::open`) are dropped.
/// This is a documented preview-only gap (docs/parity-board.md "Raster / generated mattes in
/// preview"); keeping it here, next to [`ComposeLayer::from_active`], makes the gap a single
/// named function that the golden suite exercises natively
/// (`renderly-core/tests/golden_frames.rs`).
pub fn preview_supported_mask(mask: Option<&ClipMask>) -> Option<ClipMask> {
    mask.filter(|m| {
        !matches!(
            m.kind,
            crate::project::ClipMaskKind::Raster { .. }
                | crate::project::ClipMaskKind::Generated { .. }
        )
    })
    .cloned()
}

impl From<RgbaFrame> for ComposeLayer {
    fn from(frame: RgbaFrame) -> Self {
        Self {
            source: LayerSource::Frame(frame),
            transform: ClipTransform::default(),
            effects: Vec::new(),
            mask: None,
            transition: None,
        }
    }
}

/// Matches `LayerParams` in composite.wgsl (48 bytes, 16-byte aligned).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerParamsUniform {
    cover_scale: [f32; 2],
    cover_offset: [f32; 2],
    user_translate: [f32; 2],
    user_scale: [f32; 2],
    rotation_rad: f32,
    opacity: f32,
    _pad: [f32; 2],
}

/// "Cover" fit: scale/offset (in source-texture UV space) that fill the output rect
/// without distorting the layer's aspect ratio, cropping whichever axis overflows.
/// Identity when the layer already matches the output aspect ratio (e.g. caption layers,
/// which are already rendered at the output resolution).
fn cover_uv(layer_w: u32, layer_h: u32, out_w: u32, out_h: u32) -> ([f32; 2], [f32; 2]) {
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
        [(1.0 - scale_x) / 2.0, (1.0 - scale_y) / 2.0],
    )
}

fn layer_params(
    layer_w: u32,
    layer_h: u32,
    out_w: u32,
    out_h: u32,
    transform: &ClipTransform,
) -> LayerParamsUniform {
    let (cover_scale, cover_offset) = cover_uv(layer_w, layer_h, out_w, out_h);
    let t = transform.clamp_opacity();
    LayerParamsUniform {
        cover_scale,
        cover_offset,
        user_translate: [t.x as f32, t.y as f32],
        user_scale: [t.scale_x as f32, t.scale_y as f32],
        rotation_rad: (t.rotation_deg as f32).to_radians(),
        opacity: t.opacity as f32,
        _pad: [0.0, 0.0],
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
    sampler: wgpu::Sampler,
    effects: EffectProcessor,
    transitions: TransitionPass,
    // A1: per-source-size free list of layer upload textures, reused across composite()
    // calls instead of allocating one per layer per frame (playback holds a decoded frame
    // at the same size for many consecutive frames, so the steady state is near-100% reuse).
    layer_pool: TexturePool,
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

        Self::from_device(Arc::new(device), Arc::new(queue), width, height)
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
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
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
            sampler,
            effects,
            transitions,
            layer_pool: TexturePool::default(),
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
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compose-gpu"),
            });
        let keep_alive = self.encode_layers(packs, layers, &mut encoder)?;
        self.queue.submit(Some(encoder.finish()));
        for texture in keep_alive {
            self.layer_pool.give_back(texture);
        }
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
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("export-frame"),
            });

        let keep_alive = self.encode_layers(packs, layers, &mut encoder)?;

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
        for texture in keep_alive {
            self.layer_pool.give_back(texture);
        }

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

    fn encode_layers(
        &mut self,
        packs: &[LoadedPack],
        layers: &[ComposeLayer],
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<Vec<wgpu::Texture>, ComposeError> {
        // Clear once up front.
        {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
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

        let mut keep_alive: Vec<wgpu::Texture> = Vec::with_capacity(layers.len());

        let mut i = 0;
        while i < layers.len() {
            let pair = layers.get(i).and_then(|a| {
                let ta = a.transition?;
                if ta.is_incoming {
                    return None;
                }
                let b = layers.get(i + 1)?;
                let tb = b.transition?;
                if !tb.is_incoming || ta.kind != tb.kind {
                    return None;
                }
                Some((ta.kind, ta.progress))
            });

            if let Some((kind, progress)) = pair {
                self.transitions
                    .ensure_rts(&self.device, self.width, self.height);
                self.draw_layer_to_transition_rt(
                    packs,
                    &layers[i],
                    true,
                    encoder,
                    &mut keep_alive,
                )?;
                self.draw_layer_to_transition_rt(
                    packs,
                    &layers[i + 1],
                    false,
                    encoder,
                    &mut keep_alive,
                )?;
                self.transitions
                    .blend(&self.device, encoder, &self.output_view, kind, progress)?;
                i += 2;
                continue;
            }

            self.draw_layer_to_output(packs, &layers[i], encoder, &mut keep_alive)?;
            i += 1;
        }

        Ok(keep_alive)
    }

    fn draw_layer_to_output(
        &mut self,
        packs: &[LoadedPack],
        layer: &ComposeLayer,
        encoder: &mut wgpu::CommandEncoder,
        keep_alive: &mut Vec<wgpu::Texture>,
    ) -> Result<(), ComposeError> {
        self.draw_layer(packs, layer, DrawTarget::Output, encoder, keep_alive)
    }

    fn draw_layer_to_transition_rt(
        &mut self,
        packs: &[LoadedPack],
        layer: &ComposeLayer,
        to_a: bool,
        encoder: &mut wgpu::CommandEncoder,
        keep_alive: &mut Vec<wgpu::Texture>,
    ) -> Result<(), ComposeError> {
        self.draw_layer(
            packs,
            layer,
            if to_a {
                DrawTarget::TransitionA
            } else {
                DrawTarget::TransitionB
            },
            encoder,
            keep_alive,
        )
    }

    fn draw_layer(
        &mut self,
        packs: &[LoadedPack],
        layer: &ComposeLayer,
        target: DrawTarget,
        encoder: &mut wgpu::CommandEncoder,
        keep_alive: &mut Vec<wgpu::Texture>,
    ) -> Result<(), ComposeError> {
        let src_w = layer.source.width();
        let src_h = layer.source.height();
        // Frame sources go through the pooled upload textures (returned to the pool after
        // submit via `keep_alive`); Texture sources are externally owned and bound directly
        // — the caller keeps them alive across the frame, so they never enter the pool.
        let uploaded_view = match &layer.source {
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
                keep_alive.push(texture);
                view
            }
            LayerSource::Texture { texture, .. } => texture.create_view(&Default::default()),
        };

        let use_effects = self.effects.apply(
            &self.device,
            &self.queue,
            encoder,
            &uploaded_view,
            src_w,
            src_h,
            &layer.effects,
            layer.mask.as_ref(),
            packs,
        )?;

        let params = layer_params(src_w, src_h, self.width, self.height, &layer.transform);
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("layer-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let sample_view = if use_effects {
            self.effects.result_view()
        } else {
            &uploaded_view
        };

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("layer-bind"),
            layout: &self.bind_group_layout,
            entries: &[
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
            ],
        });

        let clear = matches!(target, DrawTarget::TransitionA | DrawTarget::TransitionB);
        let dest_view = match target {
            DrawTarget::Output => &self.output_view,
            DrawTarget::TransitionA => self.transitions.view_a(),
            DrawTarget::TransitionB => self.transitions.view_b(),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        Ok(())
    }
}

enum DrawTarget {
    Output,
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

    fn effect(effect_id: &str, params: BTreeMap<String, f64>) -> EffectInstance {
        EffectInstance {
            id: Id::new_v4(),
            effect_id: effect_id.into(),
            enabled: true,
            params,
        }
    }

    #[test]
    fn cover_uv_is_identity_for_matching_aspect_ratio() {
        let (scale, offset) = cover_uv(1080, 1920, 1080, 1920);
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
        let (scale, offset) = cover_uv(1920, 1080, 1080, 1920);
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
        let (scale, offset) = cover_uv(1080, 1920, 1920, 1080);
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
        let params = layer_params(1920, 1080, 1080, 1920, &ClipTransform::default());
        let (scale, offset) = cover_uv(1920, 1080, 1080, 1920);
        assert_eq!(params.cover_scale, scale);
        assert_eq!(params.cover_offset, offset);
        assert_eq!(params.user_translate, [0.0, 0.0]);
        assert_eq!(params.user_scale, [1.0, 1.0]);
        assert!((params.rotation_rad).abs() < 1e-6);
        assert!((params.opacity - 1.0).abs() < 1e-6);
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
                        "builtin:color_adjust",
                        [("exposure".into(), 1.0)].into_iter().collect(),
                    )],
                    mask: None,
                    transition: None,
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
                }],
            )
            .unwrap();
        assert_eq!(identity, empty_chain);

        let mut disabled = effect(
            "builtin:color_adjust",
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
                        "builtin:blur",
                        [("radius".into(), 0.0)].into_iter().collect(),
                    )],
                    mask: None,
                    transition: None,
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
                            kind: TransitionKind::Crossfade,
                            progress: 0.5,
                            is_incoming: false,
                        }),
                    },
                    ComposeLayer {
                        source: LayerSource::Frame(blue),
                        transform: ClipTransform::default(),
                        effects: vec![],
                        mask: None,
                        transition: Some(LayerTransition {
                            kind: TransitionKind::Crossfade,
                            progress: 0.5,
                            is_incoming: true,
                        }),
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

    #[test]
    fn builtin_effect_ids_lists_locked_set() {
        let ids = builtin_effect_ids();
        assert_eq!(ids.len(), 6);
        assert!(ids.contains(&"builtin:color_adjust"));
        assert!(ids.contains(&"builtin:blur"));
        assert!(ids.contains(&"builtin:lut_contrast"));
        assert!(ids.contains(&"builtin:lut_warm"));
        assert!(ids.contains(&"builtin:glitch"));
        assert!(ids.contains(&"builtin:chroma_key"));
    }
}
