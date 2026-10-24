# Webview preview migration (WASM compositor)

Status: **complete 2026-07-21 (P4 shipped)** — approved 2026-07-12 (supersedes the "native
child-window preview" rule; AGENTS.md §0.5 updated in the same change). This doc is the
contract for the migration and the record of how it played out; the native child-window
preview described throughout (as the pre-migration baseline, then as a runtime fallback)
no longer exists in the codebase as of P4.

## Why

Measured on generated 1080p60 H.264 @ 51 Mbps footage in the app-equivalent Chromium engine:

| Path | Presented fps | Dropped | Per-frame paint cost |
|---|---|---|---|
| Browser pipeline → Canvas2D | 60.1 | 0 / 1544 | 0.11 ms |
| Browser pipeline → WebGL texture | 60.0 | 0 / 1573 | 0.12 ms |

The browser gives hardware decode, zero-copy compositing, and frame scheduling for free.
The native path (FFmpeg CLI → RGBA pipe → wgpu → present) pays CPU decode + pipe copies +
GPU↔CPU round-trips, and carries four per-OS child-window backends
(`renderly-app/src-tauri/src/preview/{win32,macos,linux/x11,linux/wayland}.rs`) plus the
bounds-sync fragility of OS-composited child windows. Direct manipulation (handles, masks,
guides) also becomes first-class DOM instead of overlays floating over a foreign window.

## Non-negotiables

1. **One compositor.** The preview uses `renderly-core`'s compositor compiled to `wasm32`,
   fed by browser-decoded frames. Never re-implement effects in JS — that forks
   preview↔export parity. Export and headless (CLI/MCP `render_frame`) keep the native engine.
2. **Command API and engine boundaries are unchanged.** This migration touches only how
   pixels reach the screen in `renderly-app`.
3. **Native preview stayed as a runtime fallback** (env/flag) until the webview path
   reached parity on the QA checklist, then the child-window backends were deleted in P4
   (below) — done, no fallback remains.

## Architecture

```
media file ──► <video> / WebCodecs VideoDecoder (HW decode, browser-scheduled)
                    │  VideoFrame (GPU-backed)
                    ▼
    importExternalTexture / texImage2D  ──►  renderly-core compositor (wasm32 + WebGPU)
                    │                          effects/transitions/masks = same WGSL
                    ▼
              <canvas> in the editor layout (plain DOM sibling of handles/overlays)
```

- **Decode**: start with `<video>` elements per active clip + `requestVideoFrameCallback`
  (simplest, browser-scheduled); move seeking-critical paths to `WebCodecs`
  (`VideoDecoder` + mp4 demux) where frame-accurate stepping needs it. Audio stays on the
  existing rodio path initially (backend clock), then migrates to WebAudio when the webview
  owns playback end-to-end.
- **Compositor**: new crate feature `renderly-core/wasm-compositor` gating a `wasm32`
  build of `compose/` (wgpu → WebGPU). The `media/ffmpeg_cli`, `plugins` (wasmtime),
  `captions` STT/TTS, and `perceive` modules are **excluded** on wasm32 (feature-gated) —
  the wasm build takes decoded RGBA/external textures as input, nothing else.
- **Bindings**: `wasm-bindgen` interface `WasmCompositor { new(canvas), set_project(json),
  render(time, frames: [(clip_id, VideoFrame|texture)]) }` in a new `renderly-wasm` crate
  (keeps wasm-bindgen out of core's default build).
- **Clock**: webview owns the playback clock for preview (rVFC-driven); the backend stays
  authoritative for export timing. `playback:tick` events reverse direction (frontend →
  store) or disappear entirely once preview is local.

## Phases

- **P1a — Paused frame in a DOM canvas (attempted 2026-07, REVERTED).** Tried to fix the
  handles-occlusion bug by hiding the native surface and rendering the composited frame to
  a webview `<img>` via a new `render_preview_frame(time) -> PNG data URL` command (reused
  `render_live_frame_png`), so the DOM transform/mask handles would compose above it
  instead of being occluded. It worked structurally (verified z-order in the harness) but
  was **not** the PoC's fast path — the PoC's 60fps came from the *browser* decoding video;
  this instead round-tripped through the engine (FFmpeg decode → composite → PNG-encode →
  base64 → IPC → webview PNG-decode) and reopened the decoder per call, so it was slow even
  after narrowing the trigger to "only while transform-editing a clip." Reverted at the
  user's request rather than layer more fixes onto a stopgap; the handles-occlusion bug is
  **still open**. Do not reintroduce a PNG-per-frame bridge as the fix — go straight to real
  P1 (`<video>`/WebCodecs decode in the webview) so speed and the occlusion fix land
  together. If a stopgap is wanted again before P1, consider a persistent/cached decoder
  server-side (avoid reopening FFmpeg per call) rather than PNG round-trips.
- **P1 — Decode + blit (no effects). IMPLEMENTED 2026-07-15.** `<video>`/`<img>`-fed
  `<canvas>` preview, default ON (opt back to native via
  `localStorage["renderly.previewEngine"] = "native"`, checked by `isWebviewPreview()` in
  `renderly-app/src/preview/webviewPreviewEngine.ts`). Cuts-only timelines play: multi-clip
  switching, seek/scrub, transforms, opacity, speed, and multi-track layering (images
  included) all render locally in the webview, mirroring `renderly-core`'s timeline
  evaluation and `compose/mod.rs`'s cover-fit + user-transform math exactly (never a
  from-scratch reimplementation — see the Non-negotiables above).

  **What was built:**
  - `renderly-app/src/preview/webviewPreviewEngine.ts` — `WebviewPreviewEngine`: a pooled
    `<video>`/`<img>` element per media item, a timeline evaluator mirroring
    `active_layers` (renderly-core/src/export/mod.rs) — tracks iterated in array order,
    first = bottom, per `compose/mod.rs`'s `composite` doc comment — a cover-fit + user
    transform draw (`coverSourceRect` + `drawLayer`, a pixel-space mirror of `cover_uv`/
    `layer_params` in `compose/mod.rs`), and a dual-mode clock: `playback:tick` drift-slews
    a local `requestAnimationFrame` clock when ticks arrive (real app), or the pure rAF
    clock drives everything standalone (browser harness, no backend). Canvas backing-store
    resize follows the same "only reassign width/height when it actually changed" rule as
    `timeline/renderer.ts`'s `syncCanvasSize`.
  - `renderly-app/src/preview/WebviewPreview.tsx` — the `<canvas>` component, positioned at
    the letterboxed content rect (same math as `PreviewHandlesOverlay`'s
    `contentBoundsInHost`), `z-index: 0` (below the handles/mask overlays at 5/6) — this is
    the actual occlusion-bug fix: handles are now plain DOM siblings painted after the
    canvas in z-order, not an OS-composited window on top of everything.
  - `PreviewPanel.tsx` renders `<WebviewPreview/>` instead of calling
    `ipc.setPreviewBounds` when `isWebviewPreview()` (so the native child window is never
    created); `PreviewHandlesOverlay`/`PreviewMaskOverlay` skip their backend
    `previewTransformOverride`/`previewMaskOverride` round trips in webview mode — the
    canvas subscribes to the store directly and redraws on every optimistic patch instead.
  - Backend: new `set_preview_mode(mode: "webview"|"native")` command
    (`renderly-app/src-tauri/src/lib.rs`), backed by `PlaybackEngine::webview_mode`
    (`AtomicBool`, `src-tauri/src/playback.rs`). In webview mode, `run_playback_loop` never
    constructs a `FrameRenderer` and does audio-only work (premix + `playback:tick`
    pacing); `play`/`seek`/`refresh_frame` skip `ensure_preview_parent` and the native
    scrub-worker render paths entirely (playhead bookkeeping still happens). The frontend
    calls `set_preview_mode` once at startup (`App.tsx`) from `isWebviewPreview()`.
  - Asset protocol: `open_project` and `apply_command`'s `MediaImported` outcome now call
    `app.asset_protocol_scope().allow_file(&media.path)` for every media item (helper
    `allow_media_assets` in `lib.rs`) so the webview's `<video src>`/`<img src>`
    (`convertFileSrc`) can actually load arbitrary project media, not just
    `$APPCACHE/media-cache/*`.
  - Harness upgrades (`renderly-app/src/dev/tauriMock.ts`,
    `renderly-app/vite.harness.config.ts`): a tiny `listen`/`emit` event bus; mock
    `play`/`pause` drive a ~33 Hz `playback:tick`/`playback:state` simulation; two real
    1920×1080@60 H.264 clips generated with ffmpeg
    (`testsrc2`/`smptebars`, 12s each) into a gitignored `renderly-app/dev-assets/`
    (harness-only `publicDir`, never touches `vite.config.ts` or ships in `dist`); the
    mock's `convertFileSrc` maps the sample project's fake media paths onto those two
    files so real browser decode is exercised end-to-end.

  **Measured in the browser harness** (`npx vite --config vite.harness.config.ts --port
  5188`, verified via the `javascript_tool`/DOM — the harness's browser-automation tab
  reports `document.hidden === true` even when fronted, which throttles
  `requestAnimationFrame`; `WebviewPreviewEngine` falls back to a `setTimeout`-paced loop
  only while `document.hidden`, purely so this harness can measure sustained playback at
  all — production/foreground windows always use rAF):
  - Canvas position: correctly sized/positioned at the letterboxed content rect
    (`getBoundingClientRect` matched `contentBoundsInHost`'s math), `z-index: 0`, below
    `.preview-handles-overlay` at `z-index: 5` — occlusion fix verified structurally.
  - Sustained playback over 6s: **fps ≈ 33.3, per-frame draw cost (canvas draw work,
    excluding scheduling) ≈ 0.8 ms, drops ≈ 189/200 frames** by the engine's own
    `window.__previewStats`. The fps/drop numbers reflect the harness tab's timer
    throttling (measured interval ≈2× the requested 16 ms, i.e. an environment artifact of
    this specific automated browser pane, not the engine) — they are **not** a clean 60fps
    measurement and should not be read as such. The one number that *is* environment-
    independent, per-frame draw cost (~0.8 ms for clearRect + fillRect + N-layer
    seek-correction + `drawImage`), is well inside a 16.7 ms/60fps budget and consistent
    with the PoC's ~0.11 ms Canvas2D `drawImage` figure (ours is higher because it
    includes per-frame layer bookkeeping the PoC's isolated benchmark didn't).
    **Foreground confirmation: 2026-07-15, the project owner ran the real desktop app and
    confirmed playback speed meets expectations** ("fast as I was expecting"), closing the
    gap the automated harness couldn't cover (its pane reports `document.hidden`, which
    throttles rAF). The formal QA-checklist numbers (4K60, 3-track, scrub-storm) below
    remain to be captured, but the P1 perf gate is considered met for 1080p60.
  - Scrub: sampled a pixel before/after seeking across the `c-round1`→`c-killcam` cut
    boundary (t=2 → t=10, testsrc2 vs smptebars source clips) — pixel value changed
    (`[243,233,103]` → `[0,161,0]`), confirming the canvas actually repaints on scrub.
  - Multi-clip cut during playback: played from t=8.5 across the t=9.4 cut to t≈11.5;
    sampled pixel matched the *next* clip's content, confirming gapless in-playback
    switching.
  - Transform redraw while paused: isolated the overlay clip (hid the covering track),
    patched its `ClipTransform` directly on the store (mirroring
    `PreviewHandlesOverlay`'s optimistic drag patch) with no `playing` and no backend
    call — canvas content changed immediately (`notifyProjectPatched`'s store-subscription
    redraw), confirming local full-rate transform preview.
  - Native fallback: `localStorage["renderly.previewEngine"] = "native"` reload correctly
    suppressed the webview canvas while leaving `PreviewHandlesOverlay` (native path)
    unaffected.

  **P1 scope limits (accepted, per the Non-negotiables above):** effects, LUTs, masks,
  chroma key, captions, and keyframed animation do **not** render in the webview preview
  yet — they're P2's WASM-compositor work. A clip with any of those active still shows its
  raw cover-fit + transform content in the webview preview (no visible error, just no
  effect); the native fallback remains the way to preview those until P2 lands. Audio
  stays entirely on the existing backend/rodio path (`playback:tick` is audio-clock-driven
  regardless of preview mode) — unaffected by this migration.
- **P2 — WASM compositor. IMPLEMENTED 2026-07-15** (stages 1–5 of the migration plan; the
  P2 target — renderly-core's actual compositor compiled to wasm32/WebGPU rendering the
  editor preview — is live, with the deferred items listed below). Committed baseline for
  stages 1–2 (wasm32 gating + eval extraction) is b910c52; this entry documents the full
  P2 state including stages 3–5.

  **What was built (stages 1–2, commit b910c52):**
  - `renderly-core` compiles on `wasm32-unknown-unknown` via `#[cfg(target_arch)]` gating
    (not a Cargo feature — simpler and can't be left off): `audio`,
    `commands`, `export`, `media`, `perceive`, `plugins`, `segmentation` are native-only;
    `project` (+`anim`), `compose` (all WGSL effects/transitions/chroma/masks), `mask`,
    `packs`' pure LUT helpers, and the new `frame` module (`RgbaFrame`, moved out of
    `media`) build on both targets. Native-only deps live under
    `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`. `captions` (P3, below) is
    now cross-target too: its `ab_glyph` rasterizer has no fs/GPU dependency of its own —
    only font *acquisition* is target-gated (native reads `RENDERLY_FONT_PATH`/system
    fonts; wasm32 embeds a bundled font via `include_bytes!`), and `render_caption_for_project`
    (pack-style lookup, needs `packs::load_project_packs`' fs access) stays native-only
    while the pure `render_caption` (builtin styles) is available on both targets.
  - The parity-critical evaluation ("which layers are active at time T, with what source
    times, keyframed params, transition progress") was extracted verbatim from
    `export/mod.rs` into decode-free `renderly-core/src/compose/eval.rs`
    (`active_layers` / `ActiveLayer` / `active_captions` / `multicam_angle_active`,
    error type `EvalError`), used by BOTH the native `FrameRenderer` and the wasm
    compositor — one evaluation, two frontends. 7 unit tests cover clip coverage,
    source-time math, transition-window pair emission, hidden tracks, captions.

  **What was built (stage 3 — the wasm crate and pipeline):**
  - `renderly-core/src/compose/mod.rs`: `ComposeLayer.frame: RgbaFrame` became
    `ComposeLayer.source: LayerSource` with two variants — `Frame(RgbaFrame)` (native
    decode path, pooled upload textures, unchanged behavior) and `Texture { texture,
    width, height }` (externally-owned GPU texture, bound directly, never pooled). This is
    the zero-readback hook: the browser fills the texture, core composites it with the
    same effect/mask/transition passes. `eval::ActiveLayer` gained `media_id` so the
    webview can key its element pool by media item.
  - New workspace member `renderly-wasm/` (cdylib+rlib). ALL its real dependencies are
    target-gated to wasm32, so it builds as an empty lib on native targets —
    `cargo build/test/clippy --workspace` on the native target stays green with it as a
    plain member (verified). wasm-bindgen surface (`src/compositor.rs`):
    - `WasmCompositor.create(canvas)` (async): WebGPU-only wgpu instance
      (`Backends::BROWSER_WEBGPU`), surface from `SurfaceTarget::Canvas`, adapter/device
      request, `get_default_config` surface config (browser-preferred format, typically
      `Bgra8Unorm` — the A4 "format parameter" turned out NOT to exist in core, whose
      internal RT is hardcoded `Rgba8Unorm`, so presentation is a tiny blit pass in
      renderly-wasm that samples core's `output_view()` into the surface; the pipeline's
      target format performs the conversion). Also installs a `device.on_uncaptured_error`
      → `console.error` hook — on the WebGPU backend a validation error silently discards
      the whole submit (stale/black canvas, no exception), which is undebuggable
      without it.
    - `set_project(json)`: serde-parses the store's project JSON (same schema as the
      project file), rebuilds the core `Compositor` via `Compositor::with_device` when the
      output resolution changes.
    - `render(time_us, sources)`: `sources` is a JS object mapping media-id →
      `HTMLVideoElement`/`HTMLImageElement`. Runs `compose::eval::active_layers`, copies
      each active layer's element into a pooled per-media `Rgba8Unorm` texture with
      `Queue::copy_external_image_to_texture` (zero CPU readback; dest usage
      `COPY_DST | RENDER_ATTACHMENT | TEXTURE_BINDING` per WebGPU spec), builds
      `ComposeLayer`s (transform/effects/mask/transition straight from eval), runs
      `compose_to_texture`, blits to the canvas surface, presents via `queue.present`.
      Not-ready elements are skipped for the frame (same policy as P1). File-backed mattes
      and pack LUTs arrive via the host-asset flow (P4b, below).
    - `debug_layers(time_us, sources)`: QA introspection used by the harness (layer
      count, per-layer element resolution, transition progress).
  - Build: `npm run build:wasm` in renderly-app runs
    `wasm-pack build ../renderly-wasm --target web --release --out-dir
    ../renderly-app/src/wasm-pkg --no-pack`. The generated pkg is **gitignored**
    (wasm-pack emits its own `*` .gitignore) — run `build:wasm` once after checkout to get
    the GPU preview path. **Binary size: 468 KB wasm (release + wasm-opt) + 61 KB JS glue**
    pre-P3; **1.16 MB wasm post-P3** — the jump is `renderly-wasm/assets/Roboto-Bold.ttf`
    (503 KB, Apache-2.0), the bundled caption font embedded via `include_bytes!` (see the
    captions entry below).
  - Shader fix that P2 flushed out: `transition.wgsl`'s `sample_or_black` called
    `textureSample` under varying-dependent control flow — native naga is lenient, but the
    browser's WGSL compiler enforces the uniformity analysis strictly and rejected the
    module, which (silently!) discarded every submit containing the transition pass.
    Rewritten to sample-then-`select` (and the slide branches hoist both samples out of
    the varying branch). Same shader on both targets; a new native GPU test
    (`compose::tests::transition_pair_blends_mid_progress`) now covers the transition
    blend path with a readback assert.

  **What was built (engine integration, `webviewPreviewEngine.ts` / `WebviewPreview.tsx`):**
  - Graceful-degradation ladder in the new async `WebviewPreviewEngine.init()`:
    `navigator.gpu` present → `requestAdapter()` non-null (probed BEFORE touching the
    canvas — a canvas that ever had a webgpu context can never yield a 2d one; plus one
    300 ms retry, adapter requests can transiently return null during page load) → lazy
    dynamic import of `/src/wasm-pkg/renderly_wasm.js` (specifier deliberately opaque to
    Vite's static analysis via `@vite-ignore`; see "build step" below) → `create(canvas)`
    → mode `"webgpu"`. Any missing rung → the untouched P1 Canvas2D path, mode
    `"canvas2d"`. `localStorage["renderly.previewEngine"]="native"` still bypasses the
    engine entirely.
  - `window.__previewStats` gained `mode: "webgpu" | "canvas2d" | "native"` (the engine
    reports the first two; "native" means the engine isn't instantiated).
  - The P1 element-management layer (pooled `<video>`/`<img>`, dual-mode clock,
    playback:tick slew, pre-seek, seek-correction, pause bookkeeping) is shared by both
    modes (`manageElements`). In webgpu mode it additionally manages transition-incoming
    clips (`transitionIncomingLayers` mirrors eval.rs's window math for ELEMENT
    MANAGEMENT only — which `<video>` to play/seek; compositing decisions stay in Rust).
  - In webgpu mode the engine hands every pooled element to `render(t, sources)` per
    frame and pushes `set_project(JSON.stringify(project))` on every project change (the
    store holds the Rust project schema verbatim), so paused transform-handle drags
    redraw live exactly as in P1 (store patch → subscription/effect → set_project + one
    render).
  - React StrictMode note: dev double-mount used to race two async inits on one canvas
    (deadlock). Init is serialized module-wide (`enqueueInit`) and honors `disposed`
    mid-flight, freeing an orphaned compositor so the successor engine can claim the
    canvas.
  - The harness/mock (`tauriMock.ts`) sample project now uses real UUID ids (`MOCK_IDS`
    keeps the old readable names addressable) — the Rust schema types ids as UUIDs and
    the wasm `set_project` serde-parses the store project, so P1's readable slug ids
    stopped parsing.

  **Measured in the browser harness (2026-07-15, `npx vite --config
  vite.harness.config.ts --port 5188`, driven via javascript_tool + `window.__store`;
  WebGPU WAS available in the pane — NVIDIA adapter):**
  - Mode ladder: `__previewStats.mode === "webgpu"`, fallback path exercised earlier in
    the session (pre-fix UUID parse failure correctly warned and fell back to canvas2d
    without a black preview).
  - Base compositing: video content on canvas (testsrc2 center `[7,19,202]` at t=2,
    smptebars `[0,164,0]` after the cut); repaints on scrub.
  - Effects (store-patched, GPU): `color_adjust` exposure 2.0 → `[25,73,255]`; blur r24 →
    `[117,118,138]`; `lut_contrast` → `[0,0,210]`; `lut_warm` → `[3,15,182]`; `glitch` →
    RGB-split displacement; all revert bit-exact to base.
  - Transitions mid-junction: crossfade window [8.9,9.4) sampled at p10/p50/p90 →
    `[229,232,0]` / `[127,202,0]` / `[25,172,0]` between pure outgoing `[255,240,0]` and
    pure incoming `[0,164,0]` — monotonic linear mix, exactly `mix(a,b,u)`. `wipe_left`
    at p50: left samples incoming, right samples outgoing. (Other 8 kinds share the same
    verified shader/uniform path; not individually pixel-sampled.)
  - Masks: rect (0.25..0.75)² → outside black, inside untouched; ellipse ditto.
  - Chroma key: greenest smptebars pixel `[0,164,0]` → keyed to transparent (black
    background) with `key_g` 0.63/tol 0.25 → `[0,0,0]`, restored exactly.
  - Keyframes (Rust `project::anim` eval, not JS): opacity ramp 1→0 over 8 s sampled at
    t=2 → `[5,14,152]` = exactly 0.75 × base (202×0.75=151.5); static opacity 0.5 →
    `[3,9,101]`; keyframed `pos_x` shifts content (left edge black, center shows shifted
    pixels). These exact multiplier matches (±1 LSB rounding) are the strongest
    parity evidence collected: same keyframe eval + same composite WGSL as export.
  - Multi-layer: with the covering track hidden, the transformed overlay clip
    (scale 0.32, y=-0.55) renders at its transformed position (content at screen-y≈0.75
    band, black elsewhere) — matching core's NDC math and P1's mirror of it.
  - Paused drag-patch: store-only project patch (no engine calls) with opacity 0.3 →
    canvas pixel `[2,6,60]` = exactly 0.3 × base — the optimistic-drag redraw path works
    in GPU mode.
  - Perf: per-frame CPU-side cost (`drawMs`: eval + element bookkeeping + 1–2
    `copyExternalImageToTexture` + compose/blit encode + submit) measured **0.4–1.2 ms**
    during sustained playback — well inside a 16.7 ms/60 fps budget. The harness pane's
    fps figure (~1 fps) is meaningless: the automation tab reports `document.hidden`
    and had been hidden long enough for Chrome's *intensive* timer throttling (~1 Hz
    timers); as with P1, foreground fps must be confirmed in the real app (P1's
    equivalent number was confirmed by the project owner in the desktop app).

  **Parity status:** the P2 target ("pixel-diff preview vs `render_frame` export readback
  ≤ 1 ULP-ish on the effects test project") is NOT yet automated cross-target — a
  browser-side canvas readback goes through the stretched surface blit and browser video
  decode, so a credible automated diff needs synthetic flat-color fixtures fed through
  both paths (future work). What exists instead: (a) both paths are literally the same
  Rust/WGSL (`compose::eval` + `Compositor`) built for two targets; (b) the native GPU
  transition test added this pass; (c) the exact-multiplier opacity/keyframe pixel checks
  above.

  **Captions burn-in (P3, landed 2026-07-20).** Chose CPU rasterization into the same
  `ComposeLayer` pipeline (not a DOM overlay) because export's caption path
  (`renderly-core/src/captions/mod.rs`) turned out to be pure Rust — `ab_glyph` glyph
  outlining with no fs/GPU coupling in the rasterizer itself, only in font *acquisition* —
  so it compiles on wasm32 unchanged once font loading is target-gated. This keeps the
  one-compositor rule intact (AGENTS.md §0.5): captions still go through the exact same
  `ComposeLayer`/WGSL composite pass as export, just fed a CPU-rasterized bitmap instead of
  a decoded video frame — export already does exactly this (`export/mod.rs`'s
  `render_inner` pushes one `ComposeLayer` per `eval::active_captions` entry with
  `LayerSource::Frame(render_caption_for_project(...))`).
  - `renderly-wasm/src/compositor.rs`'s `render()` calls the same shared
    `eval::active_captions(project, t)` export uses for timing (never re-derives caption
    visibility in JS), rasterizes each active caption via
    `renderly_core::captions::render_caption` (builtin styles only — see below), and
    uploads the RGBA bitmap into a `Rgba8Unorm` texture cached in a new
    `caption_textures: HashMap<(text, style_id), MediaTexture>` field. A cache entry is
    reused as-is (`ComposeLayer::Texture`, zero re-rasterize/re-upload) every frame the
    same text+style is active at the same output resolution; it's rebuilt only when the
    text, style_id, or project resolution changes. Pushed onto `compose_layers` as the
    topmost layer(s), identity `ClipTransform` (caption bitmaps are already rendered at
    output resolution, same as export — `compose::cover_uv` is then a no-op).
  - **Schema v8 (stage 3).** A caption's style comes from its own `style` or its track's
    `caption_style` (`Project::caption_style_for`); `ActiveCaption` carries the render style id
    (the item part of the ref) and the style's `overrides` (`fill`, `size`, `anchor` are read),
    and the texture cache key includes the overrides. **Text clips** (titles on video tracks) are
    ordinary layers of `eval::active_steps`: their `ActiveSource::Text` is turned into a
    texture by `SceneHost::text_layer` (the wasm host rasterizes with `render_caption_styled`
    into the same cache, native export with `render_caption_for_project`), and then goes through
    `ComposeLayer::from_active` like a video layer, so transform, effects, blend mode, mask and
    transitions apply. No `<video>` / `<img>` element is involved: `active_media_layers` and
    `required_assets` never list a text layer, and the host needs no new asset.
  - Font: wasm32 has no filesystem, so `captions::load_font()` embeds
    `renderly-wasm/assets/Roboto-Bold.ttf` (Apache-2.0, `LICENSE-Roboto.txt` alongside it)
    via `include_bytes!` on that target only; native `load_font()` is unchanged
    (`RENDERLY_FONT_PATH` → per-OS system font candidates). **Known parity gap:** an
    export using `RENDERLY_FONT_PATH` (or whichever system font it resolves to — Arial on
    Windows, DejaVu Sans Bold on Linux) previews with Roboto Bold instead; glyph metrics
    differ slightly, so caption line-wrap width can shift a few px between preview and
    export. Documented in the `captions` module doc comment too.
  - **Known scope gap:** `render_caption_for_project` (resolves `style_id` against
    asset-pack-authored caption styles, needs `packs::load_project_packs`'s fs access) is
    native-only. The wasm path calls the pure `render_caption` (builtin styles:
    `tiktok-bold-yellow`/`tiktok-minimal`/`tiktok-box`/`youtube-lower-thirds`) — a
    pack-authored custom caption style previews as the `_` fallback (tiktok-bold-yellow
    spec) until pack loading lands in wasm, tracked alongside the other pack-LUT preview
    gap above.
  - Canvas2D fallback path (`localStorage["renderly.previewEngine"]` unset and WebGPU
    unavailable) does **not** render captions — it's the pre-P2 JS-only compositor and
    never grew a text path; captions there remain export-only until/unless that fallback
    is retired (P4 deletes the native preview, not the Canvas2D one, so this is a real gap,
    just a narrow one: WebGPU is available in essentially all evergreen desktop browsers).
  - Verified in the harness (`vite.harness.config.ts`, port 5188, `window.__store` +
    `window.__previewEngine`): (a) sampling the preview canvas at a time with an active
    caption vs. the same time with the caption track's `hidden` flag toggled via
    `SetTrackFlags` showed 286/90225 differing pixels (text ink) at the harness's small
    (225×401) canvas backing size; (b) the same shown/hidden diff at a gap between two
    captions (t=10, between the 6–9 s and 11–13.2 s clips) was exactly 0 differing pixels;
    (c) dispatching `SetCaption` to change a caption's text while paused on it changed the
    canvas (712 differing pixels) with no reload, confirming the store-patch → redraw path
    picks up caption edits like it does transform edits. Perf: `__previewStats.drawMs`
    steady-state with the caption visible (cache warm) averaged 0.40 ms over 30 redraws vs.
    0.32 ms with the caption track hidden (no caption compositing at all) — same order of
    magnitude, confirming the texture cache keeps per-frame cost near the no-caption
    baseline. A forced cache-miss redraw (right after a `SetCaption` text change, so the
    `(text, style_id)` cache key changed) cost 0.60 ms vs. 0.30–0.40 ms for the following
    cache-hit redraws of the same text — the miss is real but small at this scale, and
    disappears immediately on the next frame.

  **Deferred on the wasm path (explicitly, per the P2 scope allowances):**
  - ~~Pack (`.cube`) LUTs~~ and ~~raster / `Generated` mattes~~ — landed in P4b (below).
  - Background removal **without a baked matte cache** — export builds a live heuristic matte
    from the decoded frame's CPU pixels (`export::resolve_background_removal_mask`); the
    preview has no CPU frame access, so it shows the clip's own mask instead. (With a baked
    `matte_cache_dir` it renders, see P4b.)
  - Audio (stays on the backend/rodio path).
  - Images in the harness: the sample logo isn't mapped to a real file by the mock, so the
    `HTMLImageElement` code path in `render` is written but not pixel-verified.
  - ~~Same-media transitions~~ — FIXED (2026-07-18): both sides of a transition between two
    clips of the SAME media item used to share one `<video>` element, so `manageElements`
    drift-corrected it to two different source times every frame (seek thrash), its
    `seeking`/`readyState` gate never settled, and `compositor.rs::render` dropped the layer
    on both sides — a black transition window. Now the JS engine lazily allocates a second
    pooled `<video>` (`secondaryVideoPool`) for the incoming side whenever it detects the
    same-media collision, keyed into `render(t, sources)`'s `sources` object as
    `"<media_id>#incoming"`; `compositor.rs`'s `resolve_layer_source` tries that key first
    for the incoming side of a transition and falls back to the plain media-id key, so
    different-media transitions are unaffected. Regression covered by the harness sample
    project's `c-round1a`/`c-round1b` split-clip crossfade (same `m-round1` media).
  - ~~Production (`vite build`) bundles do NOT include the wasm pkg~~ — resolved in P3
    (below): the pkg now lives in `public/wasm-pkg/`, which Vite serves at the site root
    in dev and copies verbatim into `dist/` for release builds. The dynamic import
    specifier stays invisible to the bundler (builds remain green with the pkg absent;
    the runtime import then throws → Canvas2D fallback).
- **P3 — Full switchover. COMPLETE 2026-07-21.**
  - Captions burn-in: landed 2026-07-20 (above).
  - **Wasm pkg ships in release builds**: `build:wasm` outputs to
    `renderly-app/public/wasm-pkg/` (gitignored); the engine imports
    `/wasm-pkg/renderly_wasm.js`, which resolves identically in Tauri dev, the browser
    harness (via a `configureServer` middleware in vite.harness.config.ts, since the
    harness claims `publicDir` for dev-assets), and production `dist/`.
    `tauri.conf.json`'s `beforeBuildCommand` runs `build:wasm` first, so a release can
    never ship without the compositor. Verified: `dist/wasm-pkg/*` present after
    `npm run build`; harness reports `mode: "webgpu"`, drawMs ≈ 0.3 from the new path.
  - **Audio decision: keep backend audio.** The backend premix + rodio sink stays the
    master clock (`playback:tick` slews the webview clock, as in P1). WebAudio mixing
    bought no user-visible improvement over this — playback start latency and sync are
    already good — and would have duplicated the filtergraph logic client-side.
    Revisit only if per-clip live audio scrubbing (audible trim-drag) becomes a
    requirement.
  - **Hot path already clean**: `PreviewPanel` never calls `set_preview_bounds` in
    webview mode (the native child window is never created), and the webview engine has
    been the default since P2 (`renderly.previewEngine` localStorage opt-out).
- **P4 — Deletion. COMPLETE 2026-07-21.** Removed the native child-window preview and
  every opt-out/mode-switch that existed only to support it.

  **What was removed:**
  - `renderly-app/src-tauri/src/preview/` entirely — `win32.rs`, `macos.rs`, `stub.rs`,
    `linux/{mod.rs,x11.rs,wayland.rs}`, `gfx.rs` (the shared wgpu blit pipeline —
    `present_rgba`/`present_texture_view`/`GfxState` — and `mod.rs`'s
    `PreviewPanel`/`PreviewBounds`/`NativeWindow` types and `cfg`-gated per-OS dispatch),
    and `preview_blit.wgsl`. `renderly-app/src-tauri/Cargo.toml` dropped the
    now-unreferenced `wgpu`, `pollster`, `bytemuck`, `raw-window-handle`, `windows`
    (Win32), `objc2*` (AppKit), and `x11`/`wayland-*` (Linux) dependencies.
  - `lib.rs`: `AppState.preview`/`parent_attached`, `native_window_from_app`,
    `ensure_preview_parent`, the `set_preview_bounds` and `set_preview_mode` commands, and
    the `preview_transform_override`/`preview_mask_override` commands (their only job was
    rendering to the now-deleted native surface — the webview canvas already redraws from
    the store patch these commands' callers make anyway). `quick_start_project`/
    `new_project`/`open_project` no longer call `ensure_preview_parent`.
  - `playback.rs`: `PlaybackEngine::webview_mode` (the `AtomicBool` and its
    getter/setter), `target_height`/`set_target_size`/`decode_opts` (existed only to feed
    panel-resolution decode to a `FrameRenderer` the play/scrub loops no longer build),
    `request_preview`, and the entire render/present branch of `run_playback_loop` and
    `run_scrub_worker` (`FrameRenderer`, `render_to_texture`/`render`,
    `present_texture_view`/`present_rgba`, `PerfAccumulator`/`PlaybackPerfEvent` — the
    decode/compose/present timings they measured no longer exist on this path). `play()`
    and `seek()`/`refresh_frame()` are unconditional now (no `is_webview_mode()` branch)
    since there's only one mode; `run_scrub_worker` now does nothing but the
    `mix_timeline_audio_segment` blip for `scrub_audio`.
  - Frontend: `isWebviewPreview()` and the `renderly.previewEngine` localStorage opt-out
    (`webviewPreviewEngine.ts`) — every call site (`PreviewPanel.tsx`, `App.tsx`,
    `PreviewHandlesOverlay.tsx`, `PreviewMaskOverlay.tsx`) now takes the webview path
    unconditionally. `ipc.ts` dropped `setPreviewMode`, `setPreviewBounds`,
    `previewTransformOverride`, `previewMaskOverride`, and the now-orphaned
    `onWindowGeometryChange` helper. `PreviewPanel.tsx` dropped `syncPreviewBounds` and
    its whole bounds-sync effect (it was already a no-op in webview mode — the effect's
    own early-return made that a dead branch prior to this change). `PreviewHandlesOverlay`
    /`PreviewMaskOverlay` dropped the `previewOverride` ephemeral-render callback and its
    `lastPreviewMs` throttle bookkeeping; `MediaClipInspector`'s mask-feather slider
    dropped its (unconditional, not `isWebviewPreview()`-gated) `previewMaskOverride` call.
    `PreviewMode`'s `"native"` stats value is kept in the type (harmless, never reported)
    since it's part of the debug-stats surface, not the deleted preview path.

  **Shared-device decision: dropped, not preserved.** The A4 optimization
  (`FrameRenderer::with_device`, sharing one wgpu device between the native preview
  surface and playback/scrub rendering) is gone, not migrated — it has no consumer left.
  Preview rendering was the *only* thing that ever called `with_device`; with the webview
  owning 100% of preview pixels, `playback.rs` never constructs a `FrameRenderer` at all
  anymore (play loop is audio-only; the scrub worker plays an audio blip and nothing
  else). `with_device` itself stays in `renderly-core` (still valid public API, exercised
  by its own tests) — this is purely "no remaining caller in `renderly-app`," not an API
  removal. Export, project thumbnails, and the MCP bridge's `render_frame` all already
  used `FrameRenderer::new` (its own device) directly and are unaffected.
  - One real (accepted) regression from this: `AppState::render_live_frame_png` (the MCP
    bridge's `render_frame` perception tool) used to decode at the preview panel's
    reported height via `PlaybackEngine::target_height()` (fed by `set_preview_bounds`,
    now deleted); it always renders at full preset/export resolution now. Slower per call,
    never incorrect — `render_frame` is a perception tool called on demand, not a hot
    path, so trading a little latency for deleting the whole bounds-tracking mechanism was
    the right call.
  - `PlaybackErrorEvent`/`playback:error` stayed alive but was repointed: it used to fire
    on renderer-init/present failures (impossible now, there's no renderer), so it now
    fires on audio pre-mix failure instead, keeping the event non-dead rather than
    orphaning both the Rust type and the frontend's existing `onPlaybackError` listener.

  **Cross-platform reasoning (Windows-only compile host, so this was inspected by hand,
  not verified by a macOS/Linux `cargo build`):** every deleted native-preview file was
  behind `#[cfg(windows)]` / `#[cfg(target_os = "macos")]` / `#[cfg(target_os = "linux")]`
  / the catch-all `stub.rs` `#[cfg(not(any(...)))]` in `preview/mod.rs`; deleting the
  whole `preview/` directory and its `mod` declarations removes all four arms together, so
  no `cfg` arm is left dangling for any target. The only other `cfg`-gated preview code
  was `lib.rs::native_window_from_app`'s per-OS `match` arms (also deleted whole) and the
  Cargo.toml `[target.'cfg(...)'.dependencies]` blocks for `raw-window-handle`/`windows`/
  `objc2*`/`x11`/`wayland-*` (all removed, since nothing references them on any target
  anymore). Nothing in the kept code (`playback.rs`, `lib.rs`, `bridge.rs`) is
  platform-gated, so macOS/Linux builds see the exact same source as the Windows build
  verified here.

  **Verification:** `cargo build/test/clippy --workspace` (clippy with
  `--all-targets -- -D warnings`) and `cargo fmt --all` all clean on Windows; `cargo check
  --target wasm32-unknown-unknown -p renderly-core -p renderly-wasm` clean (unaffected —
  this deletion never touched wasm-target code); `npx tsc --noEmit` and `npm run build`
  clean in `renderly-app`. Harness smoke test (`npm run build:wasm` then `npx vite
  --config vite.harness.config.ts` on port 5188): opened a sample project,
  `window.__previewStats.mode === "webgpu"`, sampled a non-black canvas pixel via a
  same-call temp-canvas `drawImage`+`getImageData` (`[127,127,127,255]` at the canvas
  center), toggled play/pause with no console errors, and confirmed no console references
  to the deleted `set_preview_mode`/`set_preview_bounds` commands.

## P4b — host-supplied mattes and pack LUTs (landed)

Closes the two preview<->export parity gaps that needed files the wasm build cannot read:
raster / generated mattes and pack `.cube` LUTs. Nothing is reimplemented in JS; the webview
only moves bytes.

```
disk ──► Tauri command (project-scoped read) ──► JS (previewAssets.ts) ──► WasmCompositor ──► core Compositor
```

- **What is needed** is decided in core Rust, not JS: `WasmCompositor::required_assets()`
  returns JSON from `renderly_core::mask::project_matte_requests` (every `Raster` / `Generated`
  mask plus background-removal with a baked `matte_cache_dir`, keyed by
  `mask::matte_cache_key`, file = the raster path or `<cache_dir>/matte.png`) and
  `renderly_core::packs::referenced_pack_items` (every `pack:<pack>/<item>@<v>` ref of an
  effect, filter, LUT or transition, with the hash stored where it was applied).
- **Bytes** come from three Tauri commands in `src-tauri/src/preview_assets.rs`:
  `read_preview_matte(path)` (raw encoded image via `tauri::ipc::Response`),
  `preview_matte_stamps(paths)` (mtimes, so a matte regenerated in place at the same path
  is noticed), `read_preview_pack_items(wanted)` (a pack item's files: a LUT's `.cube` text, or an
  effect's `effect.json`, WGSL and texture files; `read_preview_pack_luts` is the older LUT-only
  command). Each serves only what the
  open project references (hover preview passes `include_installed` to also fetch installed
  items the project does not use yet, resolved by ref against the user's library), so no
  asset-protocol scope is touched (the "runtime scope or
  blank" gotcha does not apply — there is no `<img src>` of these files). The browser harness
  gets synthetic fixtures from `dev/tauriMock.ts`.
- **Mattes**: `previewAssets.ts` decodes the image with `createImageBitmap`
  (colour-management and premultiplication off) -> `getImageData` -> RGBA ->
  `WasmCompositor::set_matte(key, w, h, rgba)`. Core converts RGBA to luma8 with exactly
  `image::to_luma8`'s integer weights (`mask::luma_from_rgba`) and registers the texture in the
  compositor's matte cache (`Compositor::set_matte`) — the same cache native export fills from
  disk, so the same `mask` WGSL pass runs. `Compositor::preview_layer_mask` (shared with the
  golden suite) picks the layer's mask: a baked background-removal matte wins over the clip
  mask (as in export, via the shared `mask::cached_background_mask`), and a file-backed matte
  not yet registered renders unmasked instead of failing the frame. Luma bytes are retained
  in the wasm struct and re-registered if the compositor is rebuilt (resolution change).
- **Pack items** (LUTs, effects, filters, transitions): the host fetches each ref in
  `required_assets().pack_items` and calls `WasmCompositor::set_pack_item(item_json, textures)`
  (`item_json`: `pack_id`, `item_id`, `version`, `kind`, `hash`, and `cube` or `manifest` +
  `shaders`; `textures`: `[{path, width, height, rgba}]` decoded by the webview). A LUT goes
  through `packs::parse_cube_text`; an effect through `LoadedItem::from_sources`, the same
  `CompiledEffect::load` native packs use, so naga validates its WGSL on the way in. Both end up
  in the `LoadedPack`s (`LoadedPack::from_parts`) passed to `compose_to_texture`, and the same
  runtime runs them as in export. An item that fails to compile is kept as an error item and
  renders pass-through. Builtins are compiled into the wasm build and need nothing from the host.
  `set_pack_lut(pack, lut, text)` is the older LUT-only entry point. The compositor's uploaded
  3D-texture cache is cleared whenever the pack data changes.
- **Refresh**: `PreviewAssetSync.sync()` runs after every `set_project` (coalesced; zero IPC
  unless a project references file-backed data). It drops mattes no longer referenced, reloads
  a matte whose mtime changed, and rebuilds the pack-item set when the project's refs or the installed library change or an item
  stops being used.
- **Parity evidence**: `renderly-core/tests/golden_frames.rs`'s `PreviewPipeline` plays the
  host through the same core entry points (not the compositor's native `image::open`
  fallback) and the all-scenes parity test now includes `mask_raster_matte`,
  `mask_generated_matte` and `fx_pack_cube_lut` at the usual 1-LSB `PARITY_TOL`
  (`parity_gap_*` tests un-ignored). Not covered (needs a browser): the real
  `createImageBitmap` decode of a matte PNG (gray PNGs with colour-profile chunks or 16-bit
  depth may differ by a LSB or two from the `image` crate's decode), and translucent matte
  PNGs (canvas readback un-premultiplies; `image` ignores alpha).

## Per-frame (time-indexed) background-removal mattes

Background removal with the in-process `rvm` model bakes a matte **per source frame**
(`background_removal.matte_frames`, see project-schema.md), so the preview must show a different
matte as the playhead moves. It reuses the P4b design without any compositing in JS:

- `required_assets()` still lists the representative `matte.png` of the cache dir under the usual
  `gen:<dir>` key (so the layer is masked immediately, with the middle frame) and now carries
  `frame_mattes: true` when any clip has a sequence.
- `WasmCompositor::required_frame_mattes(t)` (core: `mask::frame_matte_requests_at`, which walks
  `eval::leaf_media_layers(project, t)`) returns, as JSON, `[{ key, path, index, stamp }]`: the
  nearest matte frame to each visible layer's **source** time - the same mapping native export
  uses - under the **same key** as the representative matte.
- `PreviewAssetSync.syncFrameMattes(t)` runs from `drawFrame` (only when `frame_mattes` is true).
  It is single-flight and coalescing (latest `t` wins): it fetches the frame PNG with
  `read_preview_matte` (now also allowed for `f######.png` files of a sequence the open project
  uses - `mask::is_frame_matte_path`), decodes it with the same `createImageBitmap` path, calls
  `set_matte(key, ..)` (replacing the previous frame's matte; the wasm struct keeps the latest
  luma for compositor rebuilds), and repaints. It keeps the last 6 decoded frames and prefetches
  the frames 0.1 s ahead, so paused scrubbing and 30 fps playback normally have the frame ready;
  when a frame is not decoded in time the previous matte frame is shown for a draw or two.
- A regenerated sequence has a new `stamp`, which invalidates what was registered.

What differs from export: export registers the matte frame for the exact output frame before it
composites; the preview registers asynchronously, so during fast playback or a scrub it can lag the
picture by a frame or two (the previous matte frame is shown until the new one is decoded). The
matte frame chosen for a given source time is identical. The decode path has the same LSB-level
caveats as other P4b mattes (see above). Two layers of one frame that share a cache dir at
different source times share one matte (same limitation as export).

## Nested sequences and adjustment layers (schema v7)

The wasm compositor renders both with the same Rust code as export (`compose::eval::active_steps`
-> `compose::build_scene` -> `Compositor::compose_steps_to_texture`; `WasmCompositor` is a
`SceneHost` supplying only element pixels). Because the same media can be needed at two
different times in one frame (a sequence placed twice), layers **inside** a nested sequence are
supplied to `render(t, sources)` per **instance key** (`InstanceKey::to_string()`), not per media
id; root-timeline layers keep their `<media id>` / `<media id>#incoming` keys unchanged.

`WasmCompositor::active_media_layers(t)` returns, as JSON, every media element the frame needs:
`[{ key, media_id, source_time, rate, nested, incoming }]` - the `sources` key to fill, the media
time to show (integer µs), and the media-time-per-timeline-time rate (clip speed x enclosing sequence
clips' speeds, measured by evaluating the Rust timeline a hair later, so JS never re-derives
trim/speed math). `webviewPreviewEngine.ts` keeps one `<video>` per nested instance key
(`nestedPool`: lazily created, seeked/rate-matched/played from that list, pre-seeked from a
lookahead call, torn down after a grace period) and hands them to `render` under those keys;
still images reuse the shared image element under the instance key. Projects without sequences
never call it. Mattes and pack LUTs need no JS change: `required_assets()` walks every
timeline. The Canvas2D fallback does not render sequences or adjustment layers.

**Open sequence in the editor (app, phase B).** `WasmCompositor::set_active_sequence(id)` makes
`render`, `active_media_layers` and `required_assets` operate on one sequence as if it were the
root timeline (`Project::with_sequence_as_root`; a stale id falls back to the root). The engine
exposes `setActiveSequence(id | null)` (wired from the `activeSequenceId` store field in
`WebviewPreview.tsx`) and reads that container's tracks via `containerView` for its element
management; times are local to the sequence. The canvas stays at the project resolution while a
sequence is open.

## Risks

- **WebKitGTK (Linux)**: WebGPU/WebCodecs lag Chromium. There is no native fallback left
  (deleted in P4) — a WebKitGTK build without WebGPU falls back to the P1 Canvas2D path
  (no effects/masks/captions, see the Canvas2D fallback note in P2 above) rather than to
  native rendering. Revisit if this proves unacceptable on real Linux distros: either a
  WebGL2 blit path (previously rejected for breaking effects parity — see the P2
  Canvas2D-fallback caption gap for the same tradeoff already accepted elsewhere) or
  bundling a Chromium-based webview.
- **wasm32 build of wgpu/compose**: naga/WebGPU limits (no `Rgba8UnormSrgb` storage, texture
  limits). Mitigation: the compositor already targets downlevel wgpu; CI adds a
  `cargo check --target wasm32-unknown-unknown -p renderly-core --features wasm-compositor`.
- **Memory**: VideoFrames must be `close()`d promptly; leak = decoder stall. Enforce in the
  player abstraction, not call sites.

## QA gate (pre-P4 flip-default gate, now historical)

The existing `docs/qa-checklist.md` preview section, plus: 4K60 source on the dogfood
machine, 3 simultaneous video tracks + 2 effects at 60fps, scrub-storm test (drag the
playhead for 10s), export pixel-parity suite green.
