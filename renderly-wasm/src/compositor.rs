//! The WebGPU-backed preview compositor bridge. See lib.rs for the module story.
//!
//! Data flow per frame (`render`):
//! 1. `compose::eval::active_layers(project, t)` — the SAME Rust evaluation the native
//!    export uses (keyframes, speed, transitions, multicam) — decides what to draw.
//! 2. For each active layer, the corresponding browser element (from `sources`) is copied
//!    into a pooled per-media GPU texture with `Queue::copy_external_image_to_texture`
//!    (WebGPU-only zero-readback path; the browser hands the decoder output straight to
//!    the GPU).
//! 3. `Compositor::compose_to_texture` runs the standard effect/mask/transition/composite
//!    passes (same WGSL as export) into the compositor's internal RT.
//! 4. A tiny blit pass samples that RT onto the canvas surface (whose swapchain format —
//!    typically `Bgra8Unorm` on the web — the render pipeline converts to implicitly).
//!
//! Deferred on this path (documented in docs/preview-webview.md P2): pack (`.cube`) LUTs
//! (they need filesystem loads; `packs` is empty here so pack-LUT effect instances are
//! skipped by the effect processor), raster/`Generated` mattes (`image::open` — masks of
//! those kinds are dropped before composite), and background removal (needs CPU frame
//! access).
//!
//! Captions (P3, landed): `eval::active_captions(project, t)` — the same shared timing eval
//! export uses — drives which caption clips are live; `renderly_core::captions::render_caption`
//! (builtin styles only; pack-authored caption styles are a documented preview gap, see that
//! module's doc comment) rasterizes each one to an RGBA bitmap with a bundled font (no
//! filesystem on wasm32). The bitmap is uploaded once into a cached `Rgba8Unorm` texture keyed
//! on `(text, style_id)` and reused as a topmost `ComposeLayer::Texture` every frame after —
//! re-rasterized only when the text/style/output-resolution changes, not per frame.
//!
//! Same-media transitions (e.g. a split clip with a crossfade at the cut, where both sides
//! of the transition share one media item): `resolve_layer_source` below resolves the
//! incoming side's element/texture cache key from `sources["<media_id>#incoming"]` when the
//! JS engine populates it (see `webviewPreviewEngine.ts`'s `secondaryVideoPool`), falling
//! back to the plain `"<media_id>"` key otherwise — so different-media transitions are
//! unaffected.

use std::collections::HashMap;
use std::sync::Arc;

use renderly_core::compose::{
    eval, preview_supported_mask, upload_layer_pixels, ComposeLayer, Compositor, LayerSource,
};
use renderly_core::project::Project;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, HtmlImageElement, HtmlVideoElement};

const BLIT_WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    // Fullscreen triangle. UV v is flipped (NDC +Y up, texture v down).
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = positions[i];
    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>(p.x * 0.5 + 0.5, 1.0 - (p.y * 0.5 + 0.5));
    return out;
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(src, samp, in.uv).rgb, 1.0);
}
"#;

fn js_err(context: &str, detail: impl core::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("renderly-wasm: {context}: {detail}"))
}

/// Resolve which `sources` entry (and which GPU-texture cache key) an `eval::ActiveLayer`
/// should use.
///
/// Same-media transitions: when two clips of a crossfade/wipe/etc reference the SAME media
/// item (e.g. a split clip), the JS engine cannot key its element pool by media id alone for
/// both sides — see `webviewPreviewEngine.ts`'s `secondaryVideoPool` doc comment. For that
/// case it hands the incoming side's `<video>` under `"<media_id>#incoming"` in addition to
/// the plain `"<media_id>"` key (which keeps tracking the outgoing side). So: for a layer
/// that IS the incoming side of a transition (`transition.is_incoming == true`), try the
/// `#incoming` key first and fall back to the plain key when absent — different-media
/// transitions never populate `#incoming`, so they resolve the plain key unchanged, with
/// zero JS-side cost. The returned key is also used to cache/key the GPU texture
/// (`ensure_media_texture`/`media_textures`) so the two sides of a same-media transition get
/// distinct textures instead of one clobbering the other.
fn resolve_layer_source(layer: &eval::ActiveLayer, sources: &js_sys::Object) -> (String, JsValue) {
    let media_id_key = layer.media_id.to_string();
    let is_incoming = layer.transition.map(|t| t.is_incoming).unwrap_or(false);
    if is_incoming {
        let incoming_key = format!("{media_id_key}#incoming");
        let incoming_el = js_sys::Reflect::get(sources, &JsValue::from_str(&incoming_key))
            .unwrap_or(JsValue::UNDEFINED);
        if !incoming_el.is_undefined() && !incoming_el.is_null() {
            return (incoming_key, incoming_el);
        }
    }
    let element = js_sys::Reflect::get(sources, &JsValue::from_str(&media_id_key))
        .unwrap_or(JsValue::UNDEFINED);
    (media_id_key, element)
}

struct MediaTexture {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

/// One preview compositor bound to one `<canvas>`. Constructed with
/// `await WasmCompositor.create(canvas)` from JS.
#[wasm_bindgen]
pub struct WasmCompositor {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    canvas: HtmlCanvasElement,
    compositor: Option<Compositor>,
    project: Option<Project>,
    /// media-id → pooled GPU texture receiving `copy_external_image_to_texture` copies.
    /// Recreated only when the source's pixel size changes.
    media_textures: HashMap<String, MediaTexture>,
    /// (caption text, style_id) → cached rasterized-caption GPU texture. Rebuilt only when
    /// the text/style is new or the output resolution changed (tracked via the cached
    /// entry's own width/height) — NOT every frame. See the module doc's captions section.
    caption_textures: HashMap<(String, String), MediaTexture>,
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    /// Cached bind group sampling the compositor's output view; rebuilt when the
    /// compositor is (project resolution change), not per frame.
    blit_bind_group: Option<wgpu::BindGroup>,
}

#[wasm_bindgen]
impl WasmCompositor {
    /// Async constructor: requests a WebGPU adapter/device, creates a surface on `canvas`,
    /// and builds the presentation pipeline. Rejects (JS exception / rejected Promise) when
    /// WebGPU is unavailable — the JS caller treats that as "fall back to Canvas2D".
    pub async fn create(canvas: HtmlCanvasElement) -> Result<WasmCompositor, JsValue> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| js_err("create_surface", e))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| js_err("request_adapter (WebGPU unavailable?)", e))?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("renderly-preview-wasm"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| js_err("request_device", e))?;

        // Surface uncaptured GPU validation errors to the JS console — on the WebGPU
        // backend a validation failure silently discards the whole submit (the canvas just
        // shows the previous frame), which is undebuggable without this.
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| {
            web_sys::console::error_1(&JsValue::from_str(&format!(
                "renderly-wasm uncaptured GPU error: {e}"
            )));
        }));

        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let surface_config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| js_err("surface config", "surface not supported by adapter"))?;
        surface.configure(&device, &surface_config);

        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("preview-blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("preview-blit"),
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
            ],
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("preview-blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("preview-blit"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_config.format,
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
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("preview-blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Ok(WasmCompositor {
            device: Arc::new(device),
            queue: Arc::new(queue),
            surface,
            surface_config,
            canvas,
            compositor: None,
            project: None,
            media_textures: HashMap::new(),
            caption_textures: HashMap::new(),
            blit_pipeline,
            blit_layout,
            blit_sampler,
            blit_bind_group: None,
        })
    }

    /// Replace the project (serde JSON, same schema as the project file / the app store).
    /// Rebuilds the core compositor when the output resolution changes.
    pub fn set_project(&mut self, json: &str) -> Result<(), JsValue> {
        let project: Project =
            serde_json::from_str(json).map_err(|e| js_err("set_project parse", e))?;
        let (w, h) = (project.settings.width, project.settings.height);
        let needs_rebuild = match &self.compositor {
            Some(c) => c.width() != w || c.height() != h,
            None => true,
        };
        if needs_rebuild {
            let compositor =
                Compositor::with_device(Arc::clone(&self.device), Arc::clone(&self.queue), w, h)
                    .map_err(|e| js_err("compositor build", e))?;
            self.compositor = Some(compositor);
            self.blit_bind_group = None;
        }
        self.project = Some(project);
        Ok(())
    }

    /// Composite the frame at `time_secs` and present it to the canvas.
    ///
    /// `sources` is a plain JS object mapping media-id strings to the `HTMLVideoElement` /
    /// `HTMLImageElement` that currently holds that media's pixels (the P1 engine's element
    /// pool). Layers whose element is missing or not yet decodable are skipped for this
    /// frame — same policy as the P1 Canvas2D path.
    pub fn render(&mut self, time_secs: f64, sources: &js_sys::Object) -> Result<(), JsValue> {
        let Some(project) = self.project.as_ref() else {
            return Ok(());
        };
        if self.compositor.is_none() {
            return Ok(());
        }

        let layers =
            eval::active_layers(project, time_secs).map_err(|e| js_err("active_layers", e))?;
        // Copied out (not borrowed) before the loop below takes `&mut self`: `project` is a
        // borrow of `self.project` and can't stay alive across the mutable
        // `ensure_media_texture`/`ensure_caption_texture` calls that follow.
        let (out_w, out_h) = (project.settings.width, project.settings.height);
        let captions = eval::active_captions(project, time_secs);

        let mut compose_layers: Vec<ComposeLayer> = Vec::with_capacity(layers.len());
        for layer in &layers {
            let (key, element) = resolve_layer_source(layer, sources);
            if element.is_undefined() || element.is_null() {
                continue;
            }

            let (source, width, height) = if let Some(video) = element.dyn_ref::<HtmlVideoElement>()
            {
                // readyState >= 2 (HAVE_CURRENT_DATA): same gate as the Canvas2D path.
                if video.ready_state() < 2 {
                    continue;
                }
                let (w, h) = (video.video_width(), video.video_height());
                if w == 0 || h == 0 {
                    continue;
                }
                (
                    wgpu::CopyExternalImageSourceInfo {
                        source: wgpu::ExternalImageSource::HTMLVideoElement(video.clone()),
                        origin: wgpu::Origin2d::ZERO,
                        flip_y: false,
                    },
                    w,
                    h,
                )
            } else if let Some(img) = element.dyn_ref::<HtmlImageElement>() {
                if !img.complete() {
                    continue;
                }
                let (w, h) = (img.natural_width(), img.natural_height());
                if w == 0 || h == 0 {
                    continue;
                }
                (
                    wgpu::CopyExternalImageSourceInfo {
                        source: wgpu::ExternalImageSource::HTMLImageElement(img.clone()),
                        origin: wgpu::Origin2d::ZERO,
                        flip_y: false,
                    },
                    w,
                    h,
                )
            } else {
                continue;
            };

            let texture = self.ensure_media_texture(&key, width, height);
            self.queue.copy_external_image_to_texture(
                &source,
                wgpu::CopyExternalImageDestInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                    color_space: wgpu::PredefinedColorSpace::Srgb,
                    premultiplied_alpha: false,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );

            // Raster / generated mattes need filesystem decode — deferred on wasm (see the
            // module doc). Everything else (rect/ellipse shapes) passes straight through.
            // Layer wiring (transform/effects/transition) is the SAME function export uses.
            let mask = preview_supported_mask(layer.mask.as_ref());
            compose_layers.push(ComposeLayer::from_active(
                layer,
                LayerSource::Texture {
                    texture,
                    width,
                    height,
                },
                mask,
            ));
        }

        for cap in &captions {
            let texture = self.ensure_caption_texture(&cap.text, &cap.style_id, out_w, out_h)?;
            // Caption bitmaps are already rendered at output resolution (same as export,
            // see `export::mod.rs`'s FrameRenderer::render_inner) — `ComposeLayer::caption`
            // is the shared identity-transform wiring.
            compose_layers.push(ComposeLayer::caption(LayerSource::Texture {
                texture,
                width: out_w,
                height: out_h,
            }));
        }

        let compositor = self.compositor.as_mut().expect("checked above");
        compositor
            .compose_to_texture(&[], &compose_layers)
            .map_err(|e| js_err("compose", e))?;

        self.present()
    }

    /// Debug/QA introspection (harness only): what does the eval see at `time_secs`, and
    /// which sources resolve? Returns a JSON-ish summary string.
    pub fn debug_layers(&self, time_secs: f64, sources: &js_sys::Object) -> String {
        let Some(project) = self.project.as_ref() else {
            return "no project".into();
        };
        match eval::active_layers(project, time_secs) {
            Err(e) => format!("eval error: {e}"),
            Ok(layers) => {
                let mut out = String::new();
                for layer in &layers {
                    let media_id_str = layer.media_id.to_string();
                    let (key, element) = resolve_layer_source(layer, sources);
                    let found = !element.is_undefined() && !element.is_null();
                    let ready = element
                        .dyn_ref::<HtmlVideoElement>()
                        .map(|v| v.ready_state())
                        .unwrap_or(99);
                    out.push_str(&format!(
                        "[media={} resolved_key={} found={found} ready={ready} transition={:?} opacity={}] ",
                        &media_id_str[media_id_str.len() - 4..],
                        key,
                        layer
                            .transition
                            .map(|t| (t.kind.as_str(), t.progress, t.is_incoming)),
                        layer.transform.opacity,
                    ));
                }
                format!("{} layers: {out}", layers.len())
            }
        }
    }

    fn present(&mut self) -> Result<(), JsValue> {
        // Track canvas backing-store resizes (the engine reassigns canvas.width/height on
        // layout changes; the surface must be reconfigured to match or present fails).
        let (cw, ch) = (self.canvas.width().max(1), self.canvas.height().max(1));
        if cw != self.surface_config.width || ch != self.surface_config.height {
            self.surface_config.width = cw;
            self.surface_config.height = ch;
            self.surface.configure(&self.device, &self.surface_config);
        }

        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            Cst::Success(frame) | Cst::Suboptimal(frame) => frame,
            // Timeout/Occluded: skip this frame cleanly, the loop retries next tick.
            Cst::Timeout | Cst::Occluded => return Ok(()),
            // Outdated/Lost/Validation: reconfigure and retry once; on a second failure
            // skip the frame rather than erroring the whole engine.
            Cst::Outdated | Cst::Lost | Cst::Validation => {
                self.surface.configure(&self.device, &self.surface_config);
                match self.surface.get_current_texture() {
                    Cst::Success(frame) | Cst::Suboptimal(frame) => frame,
                    _ => return Ok(()),
                }
            }
        };
        let surface_view = frame.texture.create_view(&Default::default());

        if self.blit_bind_group.is_none() {
            let compositor = self.compositor.as_ref().expect("compositor set in render");
            self.blit_bind_group =
                Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("preview-blit"),
                    layout: &self.blit_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(compositor.output_view()),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                        },
                    ],
                }));
        }
        let bind_group = self.blit_bind_group.as_ref().expect("just built");

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("preview-blit"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("preview-blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blit_pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }

    fn ensure_media_texture(&mut self, key: &str, width: u32, height: u32) -> wgpu::Texture {
        let stale = self
            .media_textures
            .get(key)
            .is_none_or(|t| t.width != width || t.height != height);
        if stale {
            // COPY_DST + RENDER_ATTACHMENT are required destination usages for
            // copyExternalImageToTexture per the WebGPU spec; TEXTURE_BINDING is what the
            // compositor samples through.
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("preview-media"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.media_textures.insert(
                key.to_string(),
                MediaTexture {
                    texture,
                    width,
                    height,
                },
            );
        }
        self.media_textures[key].texture.clone()
    }

    /// Rasterize (if not already cached at this text/style/resolution) and return the GPU
    /// texture for one active caption. Cache key is `(text, style_id)`; a resolution change
    /// (project settings edited) invalidates via the width/height stored on the cached entry,
    /// same pattern as `ensure_media_texture`'s size-staleness check.
    fn ensure_caption_texture(
        &mut self,
        text: &str,
        style_id: &str,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, JsValue> {
        let key = (text.to_string(), style_id.to_string());
        let stale = self
            .caption_textures
            .get(&key)
            .is_none_or(|t| t.width != width || t.height != height);
        if stale {
            let frame = renderly_core::captions::render_caption(text, style_id, width, height)
                .map_err(|e| js_err("render_caption", e))?;
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("preview-caption"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            upload_layer_pixels(&self.queue, &texture, width, height, &frame.pixels);
            self.caption_textures.insert(
                key.clone(),
                MediaTexture {
                    texture,
                    width,
                    height,
                },
            );
        }
        Ok(self.caption_textures[&key].texture.clone())
    }
}
