//! The WebGPU-backed preview compositor bridge. See lib.rs for the module story.
//!
//! Data flow per frame (`render`):
//! 1. `compose::eval::active_steps(project, t)` — the SAME Rust evaluation the native
//!    export uses (keyframes, speed, transitions, multicam, adjustment layers, nested
//!    sequences) — decides what to draw.
//! 2. `compose::build_scene` assembles the compositor input (shared with export); this
//!    crate only supplies each media layer's pixels (`WasmHost`): the corresponding browser
//!    element (from `sources`) is copied into a pooled per-source GPU texture with
//!    `Queue::copy_external_image_to_texture` (WebGPU-only zero-readback path; the browser
//!    hands the decoder output straight to the GPU).
//! 3. `Compositor::compose_steps_to_texture` runs the standard effect/mask/transition/
//!    adjustment/nested-sequence/composite passes (same WGSL as export) into the
//!    compositor's internal RT.
//! 4. A tiny blit pass samples that RT onto the canvas surface (whose swapchain format —
//!    typically `Bgra8Unorm` on the web — the render pipeline converts to implicitly).
//!
//! Host-supplied assets (docs/preview-webview.md P4b) — wasm32 has no filesystem, so data
//! the native export reads from disk reaches the compositor through the host (the webview,
//! which gets the bytes from the Tauri backend):
//! - `required_assets()` lists, as JSON, the file-backed mattes (`Raster` / `Generated`
//!   masks and baked background-removal mattes) and pack-LUT `(pack_id, lut_id)` pairs the
//!   current project references. All path / key derivation is core Rust
//!   (`mask::project_matte_requests`, `packs::referenced_pack_luts`), not JS.
//! - `set_matte(key, w, h, rgba)` registers a browser-decoded matte; core converts RGBA to
//!   luma8 exactly like `image`'s `to_luma8` and `Compositor::preview_layer_mask` then
//!   resolves the layer's mask to it. Mattes survive a compositor rebuild (resolution
//!   change) — the luma bytes are retained here and re-registered.
//! - `required_frame_mattes(t)` lists the per-frame (time-indexed) background-removal matte each
//!   visible layer needs at `t` (core: `mask::frame_matte_requests_at`); the host registers it with
//!   `set_matte` under the same key, replacing the previous frame's (docs/preview-webview.md,
//!   "Per-frame background-removal mattes").
//! - `set_pack_lut(pack, lut, cube_text)` parses `.cube` text with core's parser and builds
//!   the `LoadedPack`s handed to `compose_to_texture`, so `pack:<id>:lut:<lut>` effects run
//!   the same `pack_lut.wgsl` pass as export.
//!
//! Still deferred: background removal *without* a baked matte cache (export derives a live
//! heuristic matte from decoded CPU pixels; the preview has none and shows the clip's own
//! mask instead — see `Compositor::preview_layer_mask`).
//!
//! Captions (P3, landed): `eval::active_captions(project, t)` — the same shared timing eval
//! export uses — drives which caption clips are live; `renderly_core::captions::render_caption`
//! (builtin styles only; pack-authored caption styles are a documented preview gap, see that
//! module's doc comment) rasterizes each one to an RGBA bitmap with a bundled font (no
//! filesystem on wasm32). The bitmap is uploaded once into a cached `Rgba8Unorm` texture keyed
//! on `(text, style_id)` and reused as a topmost `ComposeLayer::Texture` every frame after —
//! re-rasterized only when the text/style/output-resolution changes, not per frame.
//!
//! Layers inside nested sequences (schema v7): the same media can be needed at two different
//! times in one frame, so the host cannot key elements by media id alone. A layer whose
//! `InstanceKey` is nested resolves its element from `sources["<instance key>"]` (the key is
//! `InstanceKey::to_string()`); `active_media_layers(t)` tells the host exactly which
//! instance keys are live at `t` (plus the source time and playback rate each needs), so
//! the JS side manages elements without re-deriving any timeline logic.
//!
//! Same-media transitions (e.g. a split clip with a crossfade at the cut, where both sides
//! of the transition share one media item): `resolve_layer_source` below resolves the
//! incoming side's element/texture cache key from `sources["<media_id>#incoming"]` when the
//! JS engine populates it (see `webviewPreviewEngine.ts`'s `secondaryVideoPool`), falling
//! back to the plain `"<media_id>"` key otherwise — so different-media transitions are
//! unaffected.

use std::collections::HashMap;
use std::sync::Arc;

use std::collections::BTreeMap;

use renderly_core::compose::{
    build_scene, eval, upload_layer_pixels, ComposeLayer, Compositor, LayerSource,
    PreviewOverrideEntry, PreviewOverrides, SceneError, SceneHost, SceneStep,
};
use renderly_core::mask::{luma_from_rgba, project_matte_requests};
use renderly_core::packs::{parse_cube_text, referenced_pack_luts, CubeLut, LoadedPack};
use renderly_core::project::{ClipMask, Id, Project};
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
    let Some((media_id, _, _)) = layer.media() else {
        return (String::new(), JsValue::UNDEFINED);
    };
    // Inside a nested sequence the same media can be live twice in one frame, so the host
    // supplies one element per instance key (`active_media_layers` lists them).
    if layer.instance.is_nested() {
        let key = layer.instance.to_string();
        let element =
            js_sys::Reflect::get(sources, &JsValue::from_str(&key)).unwrap_or(JsValue::UNDEFINED);
        return (key, element);
    }
    let media_id_key = media_id.to_string();
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

/// Pooled per-source texture for `copy_external_image_to_texture`, recreated only when the
/// source's pixel size changes.
fn ensure_media_texture(
    device: &wgpu::Device,
    textures: &mut HashMap<String, MediaTexture>,
    key: &str,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    let stale = textures
        .get(key)
        .is_none_or(|t| t.width != width || t.height != height);
    if stale {
        // COPY_DST + RENDER_ATTACHMENT are required destination usages for
        // copyExternalImageToTexture per the WebGPU spec; TEXTURE_BINDING is what the
        // compositor samples through.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
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
        textures.insert(
            key.to_string(),
            MediaTexture {
                texture,
                width,
                height,
            },
        );
    }
    textures[key].texture.clone()
}

/// The wasm `SceneHost`: media layers come from browser elements via
/// `copy_external_image_to_texture`; everything else (adjustment steps, nested sequences) is
/// `compose::build_scene`, shared with export.
struct WasmHost<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    compositor: &'a Compositor,
    media_textures: &'a mut HashMap<String, MediaTexture>,
    sources: &'a js_sys::Object,
}

impl SceneHost for WasmHost<'_> {
    type Error = JsValue;

    fn media_layer(&mut self, layer: &eval::ActiveLayer) -> Result<Option<ComposeLayer>, JsValue> {
        let (key, element) = resolve_layer_source(layer, self.sources);
        if element.is_undefined() || element.is_null() {
            return Ok(None);
        }

        let (source, width, height) = if let Some(video) = element.dyn_ref::<HtmlVideoElement>() {
            // readyState >= 2 (HAVE_CURRENT_DATA): same gate as the Canvas2D path.
            if video.ready_state() < 2 {
                return Ok(None);
            }
            let (w, h) = (video.video_width(), video.video_height());
            if w == 0 || h == 0 {
                return Ok(None);
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
                return Ok(None);
            }
            let (w, h) = (img.natural_width(), img.natural_height());
            if w == 0 || h == 0 {
                return Ok(None);
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
            return Ok(None);
        };

        let texture = ensure_media_texture(self.device, self.media_textures, &key, width, height);
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

        // Shape / luma masks pass straight through; file-backed mattes resolve to the
        // host-registered texture (and drop to "unmasked" until registered). Layer
        // wiring (transform/effects/transition) is the SAME function export uses.
        let mask = self.compositor.preview_layer_mask(layer);
        Ok(Some(ComposeLayer::from_active(
            layer,
            LayerSource::Texture {
                texture,
                width,
                height,
            },
            mask,
        )))
    }

    fn sequence_mask(&mut self, layer: &eval::ActiveLayer) -> Option<ClipMask> {
        self.compositor.preview_layer_mask(layer)
    }
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
    /// The project as the host supplied it (always the root timeline).
    root_project: Option<Project>,
    /// Sequence the host has open in its timeline, if any. While set, `project` is
    /// `root_project` with that sequence's tracks as its root timeline, so `render` /
    /// `active_media_layers` / `required_assets` show the open container with no change to
    /// the shared evaluation code. Cleared by `set_active_sequence(None)`; an id that no
    /// longer exists in the project falls back to the root.
    active_sequence: Option<Id>,
    /// What `render` evaluates: `root_project`, or its open-sequence view, with the render-only
    /// preview overrides applied (`refresh_view`). Never saved; the project is untouched.
    project: Option<Project>,
    /// Render-only per-clip overrides (hover preview, live drags). Applied to `project` by
    /// `refresh_view`, the same projection the native `FrameRenderer` applies.
    overrides: PreviewOverrides,
    /// matte cache key -> (width, height, luma8) registered by the host; re-uploaded into a
    /// rebuilt `Compositor` (its matte cache starts empty).
    mattes: HashMap<String, (u32, u32, Vec<u8>)>,
    /// pack id -> lut id -> parsed `.cube`, as supplied by the host.
    pack_luts: BTreeMap<String, BTreeMap<String, CubeLut>>,
    /// `pack_luts` as the `LoadedPack` slice `compose_to_texture` takes.
    packs: Vec<LoadedPack>,
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
            root_project: None,
            overrides: PreviewOverrides::new(),
            active_sequence: None,
            project: None,
            mattes: HashMap::new(),
            pack_luts: BTreeMap::new(),
            packs: Vec::new(),
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
        self.root_project = Some(project);
        self.refresh_view()
    }

    /// Set (or replace) a render-only override for one clip. `json` is a core
    /// `PreviewOverrideEntry`: `{"target": "<clip id>", "transform"?, "opacity"?, "effects"?}`.
    /// Nothing is saved and nothing enters undo; the next `render` shows it.
    pub fn set_preview_override(&mut self, json: &str) -> Result<(), JsValue> {
        let entry: PreviewOverrideEntry =
            serde_json::from_str(json).map_err(|e| js_err("set_preview_override parse", e))?;
        self.overrides.set_entry(entry);
        self.refresh_view()
    }

    /// Clear the render-only override for one clip. Clearing a clip with no override is a no-op.
    pub fn clear_preview_override(&mut self, target: &str) -> Result<(), JsValue> {
        let id = target
            .parse::<Id>()
            .map_err(|e| js_err("clear_preview_override parse", e))?;
        self.overrides.clear(id);
        self.refresh_view()
    }

    /// Clear every render-only override.
    pub fn clear_all_preview_overrides(&mut self) -> Result<(), JsValue> {
        self.overrides.clear_all();
        self.refresh_view()
    }

    /// Open a nested sequence for preview (`Some(uuid)`) or return to the root timeline
    /// (`None`). Times passed to `render` / `active_media_layers` afterwards are local to
    /// that sequence. An unparsable id is an error; an id the project does not contain
    /// falls back to the root so a stale id (undo, delete) never blanks the preview. The
    /// output follows the open container's size (the sequence's own, or the project's).
    pub fn set_active_sequence(&mut self, sequence_id: Option<String>) -> Result<(), JsValue> {
        self.active_sequence = match sequence_id {
            None => None,
            Some(s) => Some(
                s.parse::<Id>()
                    .map_err(|e| js_err("set_active_sequence parse", e))?,
            ),
        };
        self.refresh_view()
    }

    /// JSON `{ "mattes": [{"key", "path"}], "pack_luts": [{"pack_id", "lut_id"}] }`: the
    /// host-supplied data the current project needs (see the module doc). Empty arrays when
    /// there is no project or it references nothing file-backed.
    pub fn required_assets(&self) -> String {
        let Some(project) = self.project.as_ref() else {
            return r#"{"mattes":[],"pack_luts":[],"frame_mattes":false}"#.to_string();
        };
        let mattes: Vec<serde_json::Value> = project_matte_requests(project)
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "key": r.key,
                    "path": r.path.to_string_lossy(),
                })
            })
            .collect();
        let pack_luts: Vec<serde_json::Value> = referenced_pack_luts(project)
            .into_iter()
            .map(|(pack_id, lut_id)| serde_json::json!({ "pack_id": pack_id, "lut_id": lut_id }))
            .collect();
        serde_json::json!({
            "mattes": mattes,
            "pack_luts": pack_luts,
            // True when some clip has a per-frame background-removal matte: the host should then
            // call `required_frame_mattes(t)` as the playhead moves.
            "frame_mattes": renderly_core::mask::project_has_frame_mattes(project),
        })
        .to_string()
    }

    /// JSON `[{"key", "path", "index", "stamp"}]`: the per-frame background-removal matte each
    /// visible layer needs at timeline time `time_secs` (the nearest matte frame to the layer's
    /// *source* time - the same mapping export uses). The host decodes `path` and registers it
    /// with `set_matte(key, ..)`, replacing the previous frame's matte under that key.
    pub fn required_frame_mattes(&self, time_secs: f64) -> String {
        let Some(project) = self.project.as_ref() else {
            return "[]".to_string();
        };
        let requests: Vec<serde_json::Value> =
            renderly_core::mask::frame_matte_requests_at(project, time_secs)
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "key": r.key,
                        "path": r.path.to_string_lossy(),
                        "index": r.index,
                        "stamp": r.stamp,
                    })
                })
                .collect();
        serde_json::Value::Array(requests).to_string()
    }

    /// Register (replace) the matte stored under `key` (a `required_assets` key) from
    /// browser-decoded straight-RGBA pixels, `width * height * 4` bytes.
    pub fn set_matte(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), JsValue> {
        if rgba.len() != width as usize * height as usize * 4 {
            return Err(js_err(
                "set_matte",
                format!("{} bytes is not {width}x{height} RGBA", rgba.len()),
            ));
        }
        let luma = luma_from_rgba(rgba);
        if let Some(compositor) = self.compositor.as_mut() {
            compositor
                .set_matte(key, width, height, &luma)
                .map_err(|e| js_err("set_matte", e))?;
        }
        self.mattes.insert(key.to_string(), (width, height, luma));
        Ok(())
    }

    /// Forget a matte (mask cleared / no longer referenced).
    pub fn remove_matte(&mut self, key: &str) {
        self.mattes.remove(key);
        if let Some(compositor) = self.compositor.as_mut() {
            compositor.remove_matte(key);
        }
    }

    /// Register (replace) one pack LUT from the text of its `.cube` file.
    pub fn set_pack_lut(
        &mut self,
        pack_id: &str,
        lut_id: &str,
        cube_text: &str,
    ) -> Result<(), JsValue> {
        let cube = parse_cube_text(cube_text)
            .map_err(|e| js_err(&format!("pack lut {pack_id}/{lut_id}"), e))?;
        self.pack_luts
            .entry(pack_id.to_string())
            .or_default()
            .insert(lut_id.to_string(), cube);
        self.rebuild_packs();
        Ok(())
    }

    /// Drop every host-supplied pack LUT (pack unloaded / project closed).
    pub fn clear_pack_luts(&mut self) {
        self.pack_luts.clear();
        self.rebuild_packs();
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
        let Some(compositor) = self.compositor.as_ref() else {
            return Ok(());
        };

        let steps =
            eval::active_steps(project, time_secs).map_err(|e| js_err("active_steps", e))?;
        // Copied out (not borrowed) before the caption loop below takes `&mut self`: `project`
        // is a borrow of `self.project` and can't stay alive across the mutable
        // `ensure_caption_texture` call that follows.
        let (out_w, out_h) = (project.settings.width, project.settings.height);
        let captions = eval::active_captions(project, time_secs);

        let mut host = WasmHost {
            device: &self.device,
            queue: &self.queue,
            compositor,
            media_textures: &mut self.media_textures,
            sources,
        };
        let mut scene = build_scene(project, steps, &mut host).map_err(|e| match e {
            SceneError::Eval(e) => js_err("build_scene", e),
            SceneError::Host(e) => e,
        })?;

        for cap in &captions {
            let texture = self.ensure_caption_texture(&cap.text, &cap.style_id, out_w, out_h)?;
            // Caption bitmaps are already rendered at output resolution (same as export,
            // see `export::mod.rs`'s FrameRenderer::render_inner) — `ComposeLayer::caption`
            // is the shared identity-transform wiring.
            scene.push(SceneStep::Layer(ComposeLayer::caption(
                LayerSource::Texture {
                    texture,
                    width: out_w,
                    height: out_h,
                },
            )));
        }

        let compositor = self.compositor.as_mut().expect("checked above");
        compositor
            .compose_steps_to_texture(&self.packs, &scene)
            .map_err(|e| js_err("compose", e))?;

        self.present()
    }

    /// JSON array describing every media element the frame at `time_secs` needs, including
    /// layers inside nested sequences: `[{ "key", "media_id", "source_time", "rate",
    /// "nested", "incoming" }]`.
    ///
    /// - `key` is the entry the host must put in `render`'s `sources` object for that layer:
    ///   the instance key for nested layers, the media id (or `<media id>#incoming`) for
    ///   layers on the root timeline.
    /// - `source_time` is the media time to show; `rate` the media seconds per timeline
    ///   second (clip speed x enclosing sequence-clip speeds, keyframed speed included),
    ///   measured by evaluating the same Rust timeline a hair later, so the host never
    ///   re-derives speed/trim logic.
    ///
    /// Empty when there is no project. The engine calls it with `time_secs` for the live set
    /// and with a time slightly ahead to pre-seek elements before a cut.
    pub fn active_media_layers(&self, time_secs: f64) -> String {
        let Some(project) = self.project.as_ref() else {
            return "[]".to_string();
        };
        const DT: f64 = 1.0 / 120.0;
        let now = match eval::leaf_media_layers(project, time_secs) {
            Ok(l) => l,
            Err(_) => return "[]".to_string(),
        };
        let later = eval::leaf_media_layers(project, time_secs + DT).unwrap_or_default();
        let out: Vec<serde_json::Value> = now
            .iter()
            .filter_map(|layer| {
                let (media_id, _, source_time) = layer.media()?;
                let incoming = layer.transition.map(|t| t.is_incoming).unwrap_or(false);
                let nested = layer.instance.is_nested();
                let key = if nested {
                    layer.instance.to_string()
                } else if incoming {
                    format!("{media_id}#incoming")
                } else {
                    media_id.to_string()
                };
                let rate = later
                    .iter()
                    .find(|l| l.instance == layer.instance)
                    .and_then(|l| l.media())
                    .map(|(_, _, t2)| (t2 - source_time) / DT)
                    .filter(|r| r.is_finite() && *r > 0.0)
                    .unwrap_or(1.0);
                Some(serde_json::json!({
                    "key": key,
                    "media_id": media_id.to_string(),
                    "source_time": source_time,
                    "rate": rate,
                    "nested": nested,
                    "incoming": incoming,
                }))
            })
            .collect();
        serde_json::Value::Array(out).to_string()
    }

    /// Debug/QA introspection (harness only): what does the eval see at `time_secs`, and
    /// which sources resolve? Returns a JSON-ish summary string.
    pub fn debug_layers(&self, time_secs: f64, sources: &js_sys::Object) -> String {
        let Some(project) = self.project.as_ref() else {
            return "no project".into();
        };
        match eval::leaf_media_layers(project, time_secs) {
            Err(e) => format!("eval error: {e}"),
            Ok(layers) => {
                let mut out = String::new();
                for layer in &layers {
                    let Some((media_id, _, _)) = layer.media() else {
                        continue;
                    };
                    let media_id_str = media_id.to_string();
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

    /// Recompute `project` from `root_project` and `active_sequence`, then rebuild the core
    /// compositor when the view's output size differs from it (the open sequence may have its
    /// own size). Matte and pack-LUT state is re-registered on a rebuild.
    fn refresh_view(&mut self) -> Result<(), JsValue> {
        let overrides = &self.overrides;
        self.project = self.root_project.as_ref().map(|root| {
            let view = self
                .active_sequence
                .and_then(|id| root.with_sequence_as_root(id))
                .unwrap_or_else(|| root.clone());
            if overrides.is_empty() {
                view
            } else {
                overrides.apply(&view).into_owned()
            }
        });
        let Some((w, h)) = self
            .project
            .as_ref()
            .map(|p| (p.settings.width, p.settings.height))
        else {
            return Ok(());
        };
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
            if let Some(compositor) = self.compositor.as_mut() {
                for (key, (mw, mh, luma)) in &self.mattes {
                    compositor
                        .set_matte(key, *mw, *mh, luma)
                        .map_err(|e| js_err("re-register matte", e))?;
                }
                compositor.clear_pack_lut_cache();
            }
        }
        Ok(())
    }

    fn rebuild_packs(&mut self) {
        self.packs = self
            .pack_luts
            .iter()
            .map(|(pack_id, luts)| LoadedPack::from_luts(pack_id, luts.clone()))
            .collect();
        if let Some(compositor) = self.compositor.as_mut() {
            compositor.clear_pack_lut_cache();
        }
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
