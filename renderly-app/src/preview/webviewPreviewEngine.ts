// Webview preview engine. Two render modes behind one element-management layer:
//
// - "webgpu" (P2): browser-decoded <video>/<img> elements are handed to renderly-core's
//   actual compositor compiled to wasm32 (renderly-wasm's WasmCompositor) — effects,
//   transitions, masks, chroma, and keyframed animation render with the SAME Rust/WGSL
//   code paths as native export (docs/preview-webview.md "Non-negotiables" #1). Frames
//   travel element → GPU texture via copyExternalImageToTexture (zero CPU readback).
// - "canvas2d" (P1 fallback): a Canvas2D mirror of core's timeline evaluation and
//   cover-fit + transform math. No effects/masks/LUTs/captions — kept as the graceful-
//   degradation path when WebGPU or the wasm module is unavailable.
//
// The element pool, dual-mode clock (backend playback:tick slew / standalone rAF),
// pre-seek, and seek-correction logic below are shared by both modes.

import type { Clip, ClipTransform, MediaClip, MediaItem, Project, Track } from "../lib/types";
import { IDENTITY_TRANSFORM, clipDurationSecs } from "../lib/types";
import { PreviewAssetSync } from "./previewAssets";
import type { PreviewAssetBackend } from "./previewAssets";

/// Minimal structural type for renderly-wasm's WasmCompositor (the real .d.ts lives in the
/// gitignored generated pkg, so we don't import its types at build time).
interface WasmCompositorLike {
  set_project(json: string): void;
  render(timeSecs: number, sources: object): void;
  // Host-supplied assets (previewAssets.ts): file-backed mattes and pack .cube LUTs.
  required_assets(): string;
  set_matte(key: string, width: number, height: number, rgba: Uint8Array): void;
  remove_matte(key: string): void;
  set_pack_lut(packId: string, lutId: string, cubeText: string): void;
  clear_pack_luts(): void;
  free(): void;
}

export type PreviewMode = "webgpu" | "canvas2d" | "native";

/// Serializes WasmCompositor creation across engine instances. React StrictMode (dev)
/// mounts each component twice: engine #1 starts its async init, is disposed, and engine
/// #2 inits on the SAME canvas — two concurrent WebGPU surface creations on one canvas
/// deadlock. Queueing init sections lets #1 finish (and immediately free its compositor
/// because it's disposed) before #2 touches the canvas.
let initQueue: Promise<void> = Promise.resolve();
function enqueueInit<T>(work: () => Promise<T>): Promise<T> {
  const run = initQueue.then(work);
  initQueue = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}

/// Debug/QA stats exposed on `window.__previewStats` in dev builds (see harness
/// verification requirements in docs/preview-webview.md). Not present in production.
export interface PreviewStats {
  fps: number;
  drawMs: number;
  drops: number;
  /// Which pixel path is live: "webgpu" (wasm compositor), "canvas2d" (P1 fallback). The
  /// "native" value is retained in the type for historical stats compatibility but is
  /// never reported — the native child-window preview was removed in P4 (see
  /// docs/preview-webview.md).
  mode: PreviewMode;
}

type ActiveLayer = {
  trackId: string;
  clip: MediaClip;
  media: MediaItem;
  sourceTimeSecs: number;
  /// True when this layer must be tracked on the SECONDARY pooled element for its media id
  /// (see `secondaryVideoPool`) rather than the primary one — set for a transition-incoming
  /// layer whose media id collides with the outgoing side's media id (a same-media split-
  /// clip transition), where both sides would otherwise fight over one `<video>` element's
  /// `currentTime` every frame. See `transitionPairFor`/`transitionIncomingLayers`.
  useSecondary?: boolean;
};

/// Mirrors `active_layers` in renderly-core/src/export/mod.rs: iterate tracks in array
/// order (first = bottom, per compose/mod.rs's `composite` doc comment), skip hidden
/// tracks / disabled clips / non-video tracks, and pick the one clip covering `t`.
function activeLayersAt(project: Project, t: number): ActiveLayer[] {
  const layers: ActiveLayer[] = [];
  for (const track of project.tracks) {
    if (track.kind !== "video" || track.hidden) continue;
    let best: MediaClip | null = null;
    for (const clip of track.clips as Clip[]) {
      if (clip.type !== "video" || !clip.enabled) continue;
      const dur = clipDurationSecs(clip);
      const end = clip.position_secs + dur;
      if (t >= clip.position_secs && t < end) {
        if (!best || clip.position_secs > best.position_secs) best = clip;
      }
    }
    if (!best) continue;
    const media = project.media.find((m) => m.id === best!.media_id);
    if (!media) continue;
    const speed = best.speed && best.speed > 0 ? best.speed : 1;
    const sourceTimeSecs = best.source_in_secs + (t - best.position_secs) * speed;
    layers.push({ trackId: track.id, clip: best, media, sourceTimeSecs });
  }
  return layers;
}

/// If `clip` is the INCOMING side of some other clip's `outgoing_transition` on the same
/// track (same adjacency test as `transitionIncomingLayers`/eval.rs), returns the outgoing
/// clip and the transition window's start time (`cut - duration`). Used so pre-seeking
/// anchors on when the blend window opens rather than the cut point itself (see
/// `upcomingLayers`), and so callers can tell whether the pair shares one media item (a
/// same-media split-clip transition — see `useSecondary` on `ActiveLayer`).
function transitionPairFor(
  track: Track,
  clip: MediaClip,
): { outgoing: MediaClip; windowStart: number } | null {
  const clips = (track.clips as Clip[])
    .filter((c): c is MediaClip => c.type === "video" && c.enabled)
    .sort((a, b) => a.position_secs - b.position_secs);
  for (let i = 0; i < clips.length; i++) {
    const v = clips[i];
    const tr = v.outgoing_transition;
    if (!tr || tr.duration_secs <= 0) continue;
    const incoming = clips[i + 1];
    if (!incoming || incoming.id !== clip.id) continue;
    const end = v.position_secs + clipDurationSecs(v);
    if (Math.abs(incoming.position_secs - end) > 0.05 && incoming.position_secs < end - 1e-6) {
      continue;
    }
    return { outgoing: v, windowStart: end - tr.duration_secs };
  }
  return null;
}

/// Look ahead a little past `t` on the same tracks to find the *next* clip about to
/// become active, so its <video> can be pre-seeked before the cut lands (gapless cuts).
///
/// BUG B fix: for a clip that is the INCOMING side of a transition, the frame that actually
/// needs to be ready is the one at the transition WINDOW's start (`cut - duration`), which
/// can be well before the clip's nominal `position_secs` for short transitions on a long
/// clip run — `transitionIncomingLayers` starts managing/seeking the element the instant the
/// window opens, so if pre-seek hasn't primed it by then the element is unready for that
/// frame and the compositor silently drops that side of the blend (looks like "transition
/// doesn't render"). Anchor the pre-seek horizon on the window start instead of the clip
/// start for such clips.
function upcomingLayers(project: Project, t: number, horizonSecs: number): ActiveLayer[] {
  const layers: ActiveLayer[] = [];
  for (const track of project.tracks) {
    if (track.kind !== "video" || track.hidden) continue;
    let best: MediaClip | null = null;
    let bestAnchor = Infinity;
    let bestUseSecondary = false;
    for (const clip of track.clips as Clip[]) {
      if (clip.type !== "video" || !clip.enabled) continue;
      const pair = transitionPairFor(track, clip);
      const anchor = pair?.windowStart ?? clip.position_secs;
      const startsIn = anchor - t;
      if (startsIn > 0 && startsIn <= horizonSecs) {
        if (!best || anchor < bestAnchor) {
          best = clip;
          bestAnchor = anchor;
          bestUseSecondary = pair != null && pair.outgoing.media_id === clip.media_id;
        }
      }
    }
    if (!best) continue;
    const media = project.media.find((m) => m.id === best!.media_id);
    if (!media) continue;
    layers.push({
      trackId: track.id,
      clip: best,
      media,
      sourceTimeSecs: best.source_in_secs,
      useSecondary: bestUseSecondary,
    });
  }
  return layers;
}

/// During a transition window [cut - d, cut), renderly-core's eval emits BOTH the outgoing
/// and the incoming clip (see compose/eval.rs). `activeLayersAt` above only returns the
/// covering (outgoing) clip, so in webgpu mode the incoming clip's element must be managed
/// separately — this mirrors eval.rs's window math for ELEMENT MANAGEMENT ONLY (which
/// <video> to play/seek); the compositing decision itself stays in Rust.
///
/// Same-media transitions (e.g. a split clip with a crossfade at the cut): when the outgoing
/// and incoming clips reference the SAME media item, `useSecondary` is set so the caller
/// tracks the incoming side on a second pooled `<video>` element (see `secondaryVideoPool`)
/// instead of sharing the primary one with the outgoing side — otherwise both sides drift-
/// correct the single shared element to two different source times every frame (seek
/// thrash), its `seeking`/`readyState` gate never settles, and the compositor drops the
/// layer on both sides (a black window). The Rust side resolves the same-keyed
/// `"<media_id>#incoming"` source when present (renderly-wasm/src/compositor.rs).
function transitionIncomingLayers(project: Project, t: number): ActiveLayer[] {
  const layers: ActiveLayer[] = [];
  for (const track of project.tracks) {
    if (track.kind !== "video" || track.hidden) continue;
    const clips = (track.clips as Clip[])
      .filter((c): c is MediaClip => c.type === "video" && c.enabled)
      .sort((a, b) => a.position_secs - b.position_secs);
    for (let i = 0; i < clips.length; i++) {
      const v = clips[i];
      const tr = v.outgoing_transition;
      if (!tr || tr.duration_secs <= 0) continue;
      const end = v.position_secs + clipDurationSecs(v);
      const windowStart = end - tr.duration_secs;
      if (t < windowStart || t >= end) continue;
      const incoming = clips[i + 1];
      if (!incoming) continue;
      if (Math.abs(incoming.position_secs - end) > 0.05 && incoming.position_secs < end - 1e-6) {
        continue;
      }
      const media = project.media.find((m) => m.id === incoming.media_id);
      if (!media) continue;
      const speed = incoming.speed && incoming.speed > 0 ? incoming.speed : 1;
      layers.push({
        trackId: track.id,
        clip: incoming,
        media,
        sourceTimeSecs: incoming.source_in_secs + (t - windowStart) * speed,
        useSecondary: v.media_id === incoming.media_id,
      });
      break;
    }
  }
  return layers;
}

/// Mirror of `cover_uv` in renderly-core/src/compose/mod.rs, expressed as a pixel-space
/// crop rect on the source instead of a UV scale/offset — equivalent math, just phrased
/// for `ctx.drawImage`'s 9-arg form.
function coverSourceRect(
  srcW: number,
  srcH: number,
  outW: number,
  outH: number,
): { sx: number; sy: number; sw: number; sh: number } {
  const srcAspect = srcW / srcH;
  const outAspect = outW / outH;
  let sw: number;
  let sh: number;
  if (srcAspect > outAspect) {
    sh = srcH;
    sw = srcH * outAspect;
  } else {
    sw = srcW;
    sh = srcW / outAspect;
  }
  return { sx: (srcW - sw) / 2, sy: (srcH - sh) / 2, sw, sh };
}

const SEEK_CORRECT_THRESHOLD_SECS = 0.08;
const PRESEEK_HORIZON_SECS = 0.5;
const DRIFT_SNAP_THRESHOLD_SECS = 0.25;
const DRIFT_SLEW_FACTOR = 0.2;
/// BUG 3 fix: a `seek()` call while playing only rebases the local clock when the target
/// is at least this far from where the clock already predicts playback to be. Ordinary
/// backend ticks re-affirm a time close to the local prediction (that's what "the clock is
/// tracking correctly" means) and must NOT re-trigger a rebase — only `onTick`'s slew
/// should touch the clock for those. A user scrub produces a large, instantaneous jump,
/// which is what this threshold is tuned to catch.
const SEEK_REBASE_THRESHOLD_SECS = 0.15;
/// After a scrub-during-playback rebase, give the <video> elements this long to actually
/// seek and settle before `manageElements`' per-frame drift correction resumes hard-seeking
/// them — without this, the rebase and the immediate next frame's drift correction (video
/// elements haven't caught up yet) fight each other and produce the reported glitch.
const SEEK_SETTLE_MS = 200;
/// How long (in `performance.now()` ms) a secondary transition-incoming `<video>` element
/// (see `secondaryVideoPool`) is kept alive after it was last needed by
/// `transitionIncomingLayers`/`upcomingLayers` before being torn down. Same-media
/// transitions are typically short (well under a second), so without this grace window a
/// re-entering scrub or a slightly-early/late frame would tear down and immediately
/// recreate the element, forcing a fresh decode each time.
const SECONDARY_TEARDOWN_GRACE_MS = 2000;

export class WebviewPreviewEngine {
  private canvas: HTMLCanvasElement;
  /// Canvas2D context — only acquired in "canvas2d" mode (a canvas can hold either a "2d"
  /// or a "webgpu" context, never both, so the mode decision in init() must come first).
  private ctx: CanvasRenderingContext2D | null = null;
  private gpu: WasmCompositorLike | null = null;
  private modeValue: PreviewMode = "canvas2d";
  private ready = false;
  private gpuRenderErrorLogged = false;
  private project: Project | null = null;
  private assetUrl: (path: string) => string;

  private videoPool = new Map<string, HTMLVideoElement>();
  private imgPool = new Map<string, HTMLImageElement>();
  /// Cleanup functions for the 'seeked'/'loadeddata' listeners attached to each pooled
  /// video (see `reconcileMediaElements`) — BUG A fix: nothing previously listened for a
  /// seek to actually finish, so a paused scrub composited whatever the mid-seek element
  /// had (often nothing → black) and never redrew once the decoded frame landed.
  private videoListenerCleanup = new Map<string, () => void>();

  /// Secondary pooled `<video>` elements, keyed by media id, used ONLY for a transition-
  /// incoming layer whose media id collides with the outgoing side's (a same-media split-
  /// clip transition — see `ActiveLayer.useSecondary`). Created lazily on first collision,
  /// never for ordinary (different-media) transitions or non-transition playback, so most
  /// media items never get a second element. Keyed into `gpu.render`'s `sources` object as
  /// `"<media_id>#incoming"` (renderly-wasm/src/compositor.rs resolves that key first for
  /// the incoming side of a transition, falling back to the plain media-id key).
  private secondaryVideoPool = new Map<string, HTMLVideoElement>();
  private secondaryListenerCleanup = new Map<string, () => void>();
  /// `performance.now()` timestamp each secondary element was last part of the managed set
  /// (active this frame or within the pre-seek horizon) — drives `SECONDARY_TEARDOWN_GRACE_MS`
  /// teardown in `manageElements`.
  private secondaryLastNeededPerf = new Map<string, number>();

  private playing = false;
  private clockBaseSecs = 0;
  private clockBasePerf = 0;
  private pausedTimeSecs = 0;

  /// `performance.now()` timestamp before which `manageElements`' per-frame drift
  /// correction should hold off — set by `seek()` right after a scrub-during-playback
  /// rebase (see `SEEK_SETTLE_MS`).
  private suppressDriftUntilPerf = 0;

  private rafId: number | null = null;
  /// True when `rafId` was scheduled via `setTimeout` rather than `requestAnimationFrame`
  /// — see `scheduleFrame`'s doc comment.
  private usingTimerFallback = false;
  private disposed = false;

  private cssW = 0;
  private cssH = 0;

  // Perf stats (dev only).
  private frameCount = 0;
  private statsWindowStart = 0;
  private lastDrawT = 0;
  private lastFrameTime = 0;
  private drops = 0;

  /// Keeps raster/generated mattes and pack LUTs registered with the wasm compositor
  /// (null until the GPU path is live, and always null on the Canvas2D fallback).
  private assets: PreviewAssetSync | null = null;
  private readonly assetBackend: PreviewAssetBackend | null;

  constructor(
    canvas: HTMLCanvasElement,
    assetUrl: (path: string) => string,
    assetBackend: PreviewAssetBackend | null = null,
  ) {
    this.canvas = canvas;
    this.assetUrl = assetUrl;
    this.assetBackend = assetBackend;
    this.statsWindowStart = performance.now();
  }

  get mode(): PreviewMode {
    return this.modeValue;
  }

  /// Decide the render mode (graceful-degradation ladder, docs/preview-webview.md):
  /// WebGPU adapter available AND the wasm module loads AND WasmCompositor::create
  /// succeeds → "webgpu"; any missing rung → the P1 Canvas2D path. Must run before the
  /// first draw — no context is acquired until the ladder resolves (see `ctx` doc).
  async init(): Promise<void> {
    return enqueueInit(() => this.initInner());
  }

  private async initInner(): Promise<void> {
    if (this.disposed) return;
    try {
      // Feature-detect BEFORE touching the canvas: requestAdapter() on a spare probe
      // keeps the real canvas 2d-capable when WebGPU is absent/denied (a canvas that has
      // ever produced a "webgpu" context can never produce a "2d" one).
      const gpuApi = (navigator as { gpu?: { requestAdapter(): Promise<unknown | null> } }).gpu;
      let adapter = gpuApi ? await gpuApi.requestAdapter() : null;
      if (gpuApi && !adapter) {
        // requestAdapter can transiently return null during page-load GPU contention
        // (observed in the automated harness pane) — one short retry before giving up.
        await new Promise((r) => setTimeout(r, 300));
        adapter = await gpuApi.requestAdapter();
      }
      if (!adapter) {
        console.warn(
          "[preview] WebGPU adapter unavailable (navigator.gpu " +
            (gpuApi ? "present" : "missing") +
            ") — falling back to Canvas2D",
        );
      }
      if (adapter) {
        // The pkg lives in public/wasm-pkg (P3): Vite serves public/ at the site root in
        // dev (real-app dev + browser harness) and copies it verbatim into dist/ for
        // production, so one URL resolves in every mode, and
        // tauri.conf.json's beforeBuildCommand runs build:wasm first so a release can
        // never ship without it. If it's somehow absent the import throws → Canvas2D
        // fallback (see docs/preview-webview.md P3).
        //
        // The specifier MUST be built from `location.origin` at runtime rather than
        // written as a literal: Vite resolves even `@vite-ignore`'d dynamic imports whose
        // specifier it can constant-fold, and files under public/ deliberately bypass the
        // transform pipeline — so a literal "/wasm-pkg/..." fails dev with "This file is
        // in /public and ... should not be imported from source code." An absolute URL
        // assembled at runtime is opaque to that analysis and is fetched straight from the
        // server/bundle by the browser.
        const spec = `${location.origin}/wasm-pkg/renderly_wasm.js`;
        const mod = (await import(/* @vite-ignore */ spec)) as {
          default: () => Promise<unknown>;
          WasmCompositor: { create(canvas: HTMLCanvasElement): Promise<WasmCompositorLike> };
        };
        await mod.default();
        if (this.disposed) return; // unmounted while the module loaded
        this.gpu = await mod.WasmCompositor.create(this.canvas);
        this.modeValue = "webgpu";
      }
    } catch (err) {
      console.warn("[preview] WebGPU path unavailable, falling back to Canvas2D:", err);
      this.gpu = null;
    }
    if (this.disposed) {
      // Disposed while awaiting (e.g. React StrictMode's immediate unmount): release the
      // GPU surface right away so the successor engine can claim the canvas.
      if (this.gpu) {
        try {
          this.gpu.free();
        } catch {
          /* already freed */
        }
        this.gpu = null;
      }
      return;
    }
    if (this.gpu && this.project) {
      // Outside the ladder try/catch: once the canvas has a webgpu context it can never
      // produce a 2d one, so a bad project JSON must NOT flip us onto the (impossible)
      // Canvas2D rung — log it and keep the GPU surface alive for the next set_project.
      try {
        this.gpu.set_project(JSON.stringify(this.project));
      } catch (err) {
        console.error("[preview] initial set_project failed:", err);
      }
    }
    if (this.gpu && this.assetBackend) {
      this.assets = new PreviewAssetSync(
        this.gpu,
        this.assetBackend,
        () => this.drawFrame(this.currentTimeSecs()),
        undefined,
        () => this.project?.asset_pack_paths ?? [],
      );
      void this.assets.sync();
    }
    if (!this.gpu) {
      const ctx = this.canvas.getContext("2d");
      if (!ctx) throw new Error("2d context unavailable");
      this.ctx = ctx;
      this.modeValue = "canvas2d";
      this.applyCanvasTransform();
    }
    this.ready = true;
    this.drawFrame(this.currentTimeSecs());
  }

  private applyCanvasTransform(): void {
    if (!this.ctx) return;
    const dpr = window.devicePixelRatio || 1;
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }

  /// Resize the backing bitmap. Only reassigns `canvas.width`/`.height` when the target
  /// actually changed — see the identical rationale in timeline/renderer.ts's
  /// `syncCanvasSize`: writing those properties (even to their current value) discards
  /// and reallocates the bitmap, which would be a needless GPU/CPU cost done every frame
  /// if called from the draw loop instead of a resize observer.
  setSize(cssW: number, cssH: number): void {
    const dpr = window.devicePixelRatio || 1;
    const targetW = Math.max(1, Math.round(cssW * dpr));
    const targetH = Math.max(1, Math.round(cssH * dpr));
    if (this.canvas.width !== targetW || this.canvas.height !== targetH) {
      this.canvas.width = targetW;
      this.canvas.height = targetH;
      // In webgpu mode the wasm side reconfigures its surface to the new backing size on
      // the next present (WasmCompositor::present tracks canvas.width/height).
    }
    this.applyCanvasTransform();
    this.cssW = cssW;
    this.cssH = cssH;
    // Repaint immediately at the current time so a resize doesn't show a stale frame.
    this.drawFrame(this.currentTimeSecs());
  }

  setProject(project: Project | null): void {
    this.project = project;
    this.reconcileMediaElements();
    if (this.gpu && project) {
      // Same serde schema as the project file — the store holds the Rust project verbatim.
      try {
        this.gpu.set_project(JSON.stringify(project));
        // Mattes / pack LUTs the new project references (no-op unless one changed).
        void this.assets?.sync();
      } catch (err) {
        console.error("[preview] set_project failed:", err);
      }
    }
    this.drawFrame(this.currentTimeSecs());
  }

  private reconcileMediaElements(): void {
    const wanted = new Set<string>();
    for (const media of this.project?.media ?? []) {
      if (media.kind === "video") {
        wanted.add(media.id);
        if (!this.videoPool.has(media.id)) {
          const el = document.createElement("video");
          el.muted = true;
          el.preload = "auto";
          el.playsInline = true;
          el.crossOrigin = "anonymous";
          el.src = this.assetUrl(media.path);
          el.style.display = "none";
          document.body.appendChild(el);
          this.videoPool.set(media.id, el);
          // BUG A fix: redraw once a paused scrub's seek actually lands a decoded frame,
          // and once metadata/data first becomes available (covers the case where the
          // element wasn't seekable yet when `manageElements` first set currentTime).
          const onReady = () => {
            if (this.disposed || this.playing) return;
            this.drawFrame(this.pausedTimeSecs);
          };
          el.addEventListener("seeked", onReady);
          el.addEventListener("loadeddata", onReady);
          this.videoListenerCleanup.set(media.id, () => {
            el.removeEventListener("seeked", onReady);
            el.removeEventListener("loadeddata", onReady);
          });
        }
      } else if (media.kind === "image") {
        wanted.add(media.id);
        if (!this.imgPool.has(media.id)) {
          const el = new Image();
          el.src = this.assetUrl(media.path);
          this.imgPool.set(media.id, el);
        }
      }
    }
    for (const [id, el] of this.videoPool) {
      if (!wanted.has(id)) {
        this.videoListenerCleanup.get(id)?.();
        this.videoListenerCleanup.delete(id);
        el.pause();
        el.removeAttribute("src");
        el.load();
        el.remove();
        this.videoPool.delete(id);
      }
    }
    for (const id of this.imgPool.keys()) {
      if (!wanted.has(id)) this.imgPool.delete(id);
    }
    for (const id of Array.from(this.secondaryVideoPool.keys())) {
      if (!wanted.has(id)) this.teardownSecondaryVideo(id);
    }
  }

  /// Lazily create (or return the existing) secondary pooled `<video>` element for
  /// `mediaId` — see `secondaryVideoPool`'s doc comment. Mirrors the primary pool's element
  /// setup in `reconcileMediaElements` (muted/hidden/appended, same 'seeked'/'loadeddata'
  /// redraw wiring) so it behaves identically from the readiness-gate/drift-correction
  /// caller's point of view.
  private ensureSecondaryVideo(mediaId: string, media: MediaItem): HTMLVideoElement {
    const existing = this.secondaryVideoPool.get(mediaId);
    if (existing) return existing;
    const el = document.createElement("video");
    el.muted = true;
    el.preload = "auto";
    el.playsInline = true;
    el.crossOrigin = "anonymous";
    el.src = this.assetUrl(media.path);
    el.style.display = "none";
    document.body.appendChild(el);
    this.secondaryVideoPool.set(mediaId, el);
    const onReady = () => {
      if (this.disposed || this.playing) return;
      this.drawFrame(this.pausedTimeSecs);
    };
    el.addEventListener("seeked", onReady);
    el.addEventListener("loadeddata", onReady);
    this.secondaryListenerCleanup.set(mediaId, () => {
      el.removeEventListener("seeked", onReady);
      el.removeEventListener("loadeddata", onReady);
    });
    return el;
  }

  private teardownSecondaryVideo(mediaId: string): void {
    this.secondaryListenerCleanup.get(mediaId)?.();
    this.secondaryListenerCleanup.delete(mediaId);
    const el = this.secondaryVideoPool.get(mediaId);
    if (el) {
      el.pause();
      el.removeAttribute("src");
      el.load();
      el.remove();
    }
    this.secondaryVideoPool.delete(mediaId);
    this.secondaryLastNeededPerf.delete(mediaId);
  }

  private currentTimeSecs(): number {
    if (!this.playing) return this.pausedTimeSecs;
    return this.clockBaseSecs + (performance.now() - this.clockBasePerf) / 1000;
  }

  play(atSecs: number): void {
    this.playing = true;
    this.clockBaseSecs = atSecs;
    this.clockBasePerf = performance.now();
    this.frameCount = 0;
    this.statsWindowStart = performance.now();
    this.lastFrameTime = 0;
    this.drops = 0;
    if (this.rafId == null) this.scheduleFrame();
  }

  /// `requestAnimationFrame` is throttled/suspended by the browser when the document is
  /// hidden (backgrounded tab or, in this repo's automated browser-pane test harness, a
  /// tab that's never actually given compositor focus — see docs/preview-webview.md
  /// "Harness verification"). That throttling is *correct* product behavior for a real
  /// backgrounded window (no point burning GPU on an invisible canvas) but would make the
  /// harness unable to measure sustained playback fps at all, so fall back to a ~60Hz
  /// `setTimeout` only while `document.hidden` is true.
  private scheduleFrame(): void {
    this.usingTimerFallback = typeof document !== "undefined" && document.hidden;
    this.rafId = this.usingTimerFallback
      ? (window.setTimeout(this.loop, 16) as unknown as number)
      : requestAnimationFrame(this.loop);
  }

  private cancelScheduledFrame(): void {
    if (this.rafId == null) return;
    if (this.usingTimerFallback) window.clearTimeout(this.rafId);
    else cancelAnimationFrame(this.rafId);
    this.rafId = null;
  }

  pause(atSecs: number): void {
    this.playing = false;
    this.pausedTimeSecs = atSecs;
    for (const v of this.videoPool.values()) v.pause();
    this.drawFrame(atSecs);
  }

  /// Backend `playback:tick` arrives ~30Hz while the audio path is the master clock.
  /// Slews the local rAF clock toward the reported time (snaps if drift is large, e.g.
  /// after a seek) rather than following it directly, so playback stays visually smooth
  /// between ticks instead of stepping. In the browser harness (no backend), no ticks
  /// ever arrive and the pure rAF clock (see `currentTimeSecs`) drives everything.
  onTick(tickSecs: number): void {
    if (!this.playing) return;
    const now = performance.now();
    const predicted = this.clockBaseSecs + (now - this.clockBasePerf) / 1000;
    const drift = tickSecs - predicted;
    this.clockBaseSecs =
      Math.abs(drift) > DRIFT_SNAP_THRESHOLD_SECS ? tickSecs : predicted + drift * DRIFT_SLEW_FACTOR;
    this.clockBasePerf = now;
  }

  /// Paused-state scrub: set the target time, seek any active video (`manageElements` sets
  /// `.currentTime`; `reconcileMediaElements` attaches 'seeked'/'loadeddata' listeners, and
  /// `manageElements` additionally arms a `requestVideoFrameCallback` per seek, either of
  /// which redraws once the actually-decoded frame lands), and draw immediately too so a
  /// cached/instant seek doesn't wait. `drawFrame` itself skips compositing (keeping the
  /// prior canvas contents) for any frame where a managed video is still mid-seek, so a
  /// scrub never presents a black/stale frame — see `manageElements`'s return value.
  ///
  /// Also handles a scrub *during* playback (BUG 3): previously this early-returned while
  /// `this.playing` without touching the clock at all, so the local rAF clock kept
  /// advancing from the pre-scrub position until the next backend `playback:tick` noticed
  /// a large drift and hard-snapped it (`onTick`) — meanwhile `manageElements` was already
  /// hard-seeking the stale-drift `<video>` elements every frame, so the stale clock, the
  /// eventual snap, and the per-frame seeks all fought each other, producing the reported
  /// glitch. Now a sufficiently large jump (a real scrub, as opposed to an ordinary tick
  /// re-affirming a time close to the local prediction) rebases the clock immediately and
  /// briefly suppresses `manageElements`' drift correction so the `<video>` elements can
  /// seek and settle without being yanked again on the very next frame.
  seek(atSecs: number): void {
    this.pausedTimeSecs = atSecs;
    if (this.playing) {
      const predicted = this.currentTimeSecs();
      if (Math.abs(atSecs - predicted) > SEEK_REBASE_THRESHOLD_SECS) {
        this.clockBaseSecs = atSecs;
        this.clockBasePerf = performance.now();
        this.suppressDriftUntilPerf = performance.now() + SEEK_SETTLE_MS;
      }
      return; // live playback frame will pick this up on its own
    }
    this.drawFrame(atSecs);
  }

  /// Called when the store's project changes while paused (e.g. a transform-handle drag
  /// patches the project optimistically) — redraw at the current time so the change is
  /// visible immediately, at full local frame rate, without any backend round trip.
  notifyProjectPatched(): void {
    if (this.playing) return;
    this.drawFrame(this.pausedTimeSecs);
  }

  private loop = (): void => {
    if (this.disposed) return;
    if (!this.playing) {
      this.rafId = null;
      return;
    }
    const t = this.currentTimeSecs();
    this.drawFrame(t);
    this.scheduleFrame();
  };

  private drawFrame(t: number): void {
    if (!this.ready) return;
    const drawStart = performance.now();

    const project = this.project;
    const active = project ? activeLayersAt(project, t) : [];
    const managedReady = project ? this.manageElements(project, t, active) : true;

    // BUG A fix (part 2): while paused, if any managed active video layer is mid-seek or
    // hasn't decoded a frame at its target time yet, do NOT composite this frame — the GPU
    // path would upload a stale/blank texture (looks like a black flash) and the Canvas2D
    // path would clear-then-skip the layer (same visible black). Leave the canvas exactly
    // as it was; the `seeked`/`loadeddata` listeners (or rVFC) attached in
    // `reconcileMediaElements`/`manageElements` re-invoke `drawFrame` at
    // `this.pausedTimeSecs` — always the LATEST scrub target, never a stale intermediate
    // one — once the real frame is ready, so scrub-storms coalesce onto the final position.
    if (!this.playing && !managedReady) {
      this.lastDrawT = performance.now() - drawStart;
      return;
    }

    if (this.gpu) {
      // GPU path: hand every pooled element to the wasm compositor keyed by media id —
      // renderly-core's eval::active_layers (the SAME evaluation export uses) decides
      // which of them are composited, with what params. Nothing timeline-shaped is
      // decided on the JS side in this mode; `active` above only drove element readiness.
      const sources: Record<string, HTMLVideoElement | HTMLImageElement> = {};
      for (const [id, video] of this.videoPool) sources[id] = video;
      // Same-media transition-incoming elements, keyed distinctly so the Rust side gets a
      // second GPU texture instead of clobbering the outgoing side's — see
      // `secondaryVideoPool`'s doc comment and compositor.rs's `render`.
      for (const [id, video] of this.secondaryVideoPool) sources[`${id}#incoming`] = video;
      for (const [id, img] of this.imgPool) sources[id] = img;
      try {
        this.gpu.render(t, sources);
        this.gpuRenderErrorLogged = false;
      } catch (err) {
        if (!this.gpuRenderErrorLogged) {
          this.gpuRenderErrorLogged = true;
          console.error("[preview] wasm render failed (will keep retrying):", err);
        }
      }
    } else if (this.ctx) {
      const { ctx, cssW, cssH } = this;
      ctx.save();
      ctx.clearRect(0, 0, cssW, cssH);
      ctx.fillStyle = "#000";
      ctx.fillRect(0, 0, cssW, cssH);
      if (project && cssW > 0 && cssH > 0) {
        for (const layer of active) {
          const transform: ClipTransform = layer.clip.transform ?? IDENTITY_TRANSFORM;
          if (layer.media.kind === "video") {
            const video = this.videoPool.get(layer.media.id);
            if (!video || video.readyState < 2) continue;
            this.drawLayer(
              video,
              video.videoWidth || layer.media.width || 1,
              video.videoHeight || layer.media.height || 1,
              transform,
            );
          } else if (layer.media.kind === "image") {
            const img = this.imgPool.get(layer.media.id);
            if (!img || !img.complete || img.naturalWidth === 0) continue;
            this.drawLayer(img, img.naturalWidth, img.naturalHeight, transform);
          }
        }
      }
      ctx.restore();
    }

    // Stats (dev only) — see docs/preview-webview.md harness verification.
    this.lastDrawT = performance.now() - drawStart;
    this.frameCount += 1;
    if (this.lastFrameTime > 0) {
      const gap = drawStart - this.lastFrameTime;
      const fps = this.project?.settings.fps || 60;
      if (this.playing && gap > (1000 / fps) * 1.5) this.drops += 1;
    }
    this.lastFrameTime = drawStart;
    const elapsed = performance.now() - this.statsWindowStart;
    if (elapsed >= 1000) {
      const fps = (this.frameCount * 1000) / elapsed;
      this.publishStats({ fps, drawMs: this.lastDrawT, drops: this.drops, mode: this.modeValue });
      this.frameCount = 0;
      this.statsWindowStart = performance.now();
    } else {
      this.publishStats({
        fps: (globalThis as { __previewStats?: PreviewStats }).__previewStats?.fps ?? 0,
        drawMs: this.lastDrawT,
        drops: this.drops,
        mode: this.modeValue,
      });
    }
  }

  /// Registers a one-off `requestVideoFrameCallback` (where supported — Chromium/Tauri's
  /// webview; Firefox lacks it) to redraw as soon as this video actually presents a new
  /// decoded frame, which fires earlier/more precisely than the 'seeked' event. The
  /// 'seeked'/'loadeddata' listeners attached in `reconcileMediaElements` remain as the
  /// fallback for browsers without rVFC and as a backstop if this callback is ever missed.
  private scheduleVideoFrameCallback(video: HTMLVideoElement): void {
    const rvfc = (
      video as HTMLVideoElement & { requestVideoFrameCallback?: (cb: () => void) => number }
    ).requestVideoFrameCallback;
    if (typeof rvfc !== "function") return;
    rvfc.call(video, () => {
      if (this.disposed || this.playing) return;
      this.drawFrame(this.pausedTimeSecs);
    });
  }

  /// Shared element management for both render modes: pre-seek upcoming clips, pause
  /// inactive videos, and keep each active video's currentTime/playbackRate/play-state in
  /// sync with the timeline. In webgpu mode this ALSO covers transition-incoming clips
  /// (see `transitionIncomingLayers`) so both sides of a WGSL transition blend have live
  /// decoded frames.
  ///
  /// Returns whether every managed ACTIVE video layer (i.e. one that will actually be
  /// composited this frame — not pre-seek lookahead) currently has a decoded frame ready at
  /// its target time (`readyState >= 2` and not mid-seek). BUG A fix: the caller uses this to
  /// skip compositing rather than presenting a black/stale frame while a paused scrub's seek
  /// is still in flight.
  private manageElements(project: Project, t: number, active: ActiveLayer[]): boolean {
    const incoming = this.gpu ? transitionIncomingLayers(project, t) : [];
    const managed = this.gpu ? [...active, ...incoming] : active;
    const upcoming = upcomingLayers(project, t, PRESEEK_HORIZON_SECS);
    // Pausing/activeness bookkeeping below is keyed by media id (not element identity): an
    // outgoing layer and a colliding same-media incoming layer share a media id, so this set
    // still correctly keeps the PRIMARY element unpaused for both — only the incoming side
    // is redirected onto the secondary element (see `useSecondary` below).
    const activeIds = new Set(managed.map((l) => l.media.id));
    // See `seek()`'s doc comment: right after a scrub-during-playback rebase, hold off on
    // per-frame drift correction for a moment so the just-triggered `<video>` seeks can
    // land before we re-measure drift against them.
    const suppressDrift = performance.now() < this.suppressDriftUntilPerf;
    const now = performance.now();

    // Track which secondary elements are needed this frame (active transition-incoming) or
    // within the pre-seek horizon, and tear down any others that have been idle past the
    // grace window (see `SECONDARY_TEARDOWN_GRACE_MS`).
    const neededSecondary = new Set<string>();
    for (const l of incoming) if (l.useSecondary) neededSecondary.add(l.media.id);
    for (const l of upcoming) if (l.useSecondary) neededSecondary.add(l.media.id);
    for (const id of neededSecondary) this.secondaryLastNeededPerf.set(id, now);
    for (const id of Array.from(this.secondaryVideoPool.keys())) {
      if (neededSecondary.has(id)) continue;
      const lastNeeded = this.secondaryLastNeededPerf.get(id) ?? 0;
      if (now - lastNeeded > SECONDARY_TEARDOWN_GRACE_MS) this.teardownSecondaryVideo(id);
    }

    // Pre-seek upcoming clips' videos so the cut is gapless.
    for (const upcomingLayer of upcoming) {
      if (upcomingLayer.useSecondary) {
        const video = this.ensureSecondaryVideo(upcomingLayer.media.id, upcomingLayer.media);
        if (Math.abs(video.currentTime - upcomingLayer.sourceTimeSecs) > SEEK_CORRECT_THRESHOLD_SECS) {
          try {
            video.currentTime = upcomingLayer.sourceTimeSecs;
            if (!this.playing) this.scheduleVideoFrameCallback(video);
          } catch {
            /* not seekable yet (metadata not loaded) — next frame will retry */
          }
        }
        continue;
      }
      const video = this.videoPool.get(upcomingLayer.media.id);
      if (!video || activeIds.has(upcomingLayer.media.id)) continue;
      if (Math.abs(video.currentTime - upcomingLayer.sourceTimeSecs) > SEEK_CORRECT_THRESHOLD_SECS) {
        try {
          video.currentTime = upcomingLayer.sourceTimeSecs;
          if (!this.playing) this.scheduleVideoFrameCallback(video);
        } catch {
          /* not seekable yet (metadata not loaded) — next frame will retry */
        }
      }
    }

    // Pause any video that isn't part of this frame's active set.
    for (const [id, video] of this.videoPool) {
      if (!activeIds.has(id) && !video.paused) video.pause();
    }
    // Same for secondary elements no longer needed at all (not even within the pre-seek
    // horizon) — they were torn down above, but one still mid-grace-window should also pause
    // once it's no longer this frame's active incoming layer.
    for (const [id, video] of this.secondaryVideoPool) {
      const isActiveIncoming = incoming.some((l) => l.useSecondary && l.media.id === id);
      if (!isActiveIncoming && !video.paused) video.pause();
    }

    let allReady = true;
    for (const layer of managed) {
      if (layer.media.kind !== "video") continue;
      const video = layer.useSecondary
        ? this.ensureSecondaryVideo(layer.media.id, layer.media)
        : this.videoPool.get(layer.media.id);
      if (!video || video.readyState < 2) {
        allReady = false;
        continue;
      }
      const speed = layer.clip.speed && layer.clip.speed > 0 ? layer.clip.speed : 1;
      if (video.playbackRate !== speed) video.playbackRate = speed;
      const drift = Math.abs(video.currentTime - layer.sourceTimeSecs);
      if (!suppressDrift && drift > SEEK_CORRECT_THRESHOLD_SECS) {
        try {
          video.currentTime = layer.sourceTimeSecs;
          if (!this.playing) this.scheduleVideoFrameCallback(video);
        } catch {
          /* ignore */
        }
      }
      if (this.playing && video.paused) void video.play().catch(() => {});
      if (!this.playing && !video.paused) video.pause();
      // Re-check AFTER the possible currentTime write above: assigning `.currentTime` sets
      // `.seeking` synchronously per spec, so this catches the seek we may have just
      // triggered, not just one already in flight from a previous frame.
      if (video.seeking) allReady = false;
    }
    return allReady;
  }

  private drawLayer(
    source: CanvasImageSource,
    srcW: number,
    srcH: number,
    transform: ClipTransform,
  ): void {
    const { ctx, cssW, cssH } = this;
    if (!ctx) return;
    const { sx, sy, sw, sh } = coverSourceRect(srcW, srcH, cssW, cssH);
    ctx.save();
    ctx.globalAlpha = Math.max(0, Math.min(1, transform.opacity));
    ctx.translate(cssW / 2, cssH / 2);
    ctx.translate((transform.x * cssW) / 2, (-transform.y * cssH) / 2);
    // Screen-space rotation is the negation of the NDC (+Y up) rotation — see the
    // identical note (`svgAngle = -transform.rotation_deg`) in PreviewHandlesOverlay.tsx.
    ctx.rotate((-transform.rotation_deg * Math.PI) / 180);
    ctx.scale(transform.scale_x, transform.scale_y);
    ctx.drawImage(source, sx, sy, sw, sh, -cssW / 2, -cssH / 2, cssW, cssH);
    ctx.restore();
  }

  private publishStats(stats: PreviewStats): void {
    if ((import.meta as { env?: { DEV?: boolean } }).env?.DEV) {
      (globalThis as unknown as { __previewStats?: PreviewStats }).__previewStats = stats;
    }
  }

  dispose(): void {
    this.disposed = true;
    this.assets?.dispose();
    this.assets = null;
    this.cancelScheduledFrame();
    if (this.gpu) {
      try {
        this.gpu.free();
      } catch {
        /* already freed */
      }
      this.gpu = null;
    }
    for (const cleanup of this.videoListenerCleanup.values()) cleanup();
    this.videoListenerCleanup.clear();
    for (const v of this.videoPool.values()) {
      v.pause();
      v.removeAttribute("src");
      v.load();
      v.remove();
    }
    this.videoPool.clear();
    for (const id of Array.from(this.secondaryVideoPool.keys())) this.teardownSecondaryVideo(id);
    this.imgPool.clear();
  }
}
