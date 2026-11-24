// The render worker: owns the OffscreenCanvas and its WebGL2 context, receives frames straight
// from a decode worker over a MessagePort, and puts the frame due at the playhead on screen.
//
// Until the audio clock exists (S0.4), playback runs on the worker's display refresh. The clock
// starts once the decoder is primed and the display is steady (see PREROLL_FRAMES).

import {
  DecodeClient,
  DEFAULT_MAX_FRAMES_IN_FLIGHT,
  FLICKS_PER_SECOND,
  type DecodedFrame,
  type Endpoint,
} from "@renderly/media";
import * as THREE from "three";
import { FrameQueue } from "./frame-queue";
import { FrameUploader } from "./frame-upload";
import { makeTarget } from "./gpu";
import { containRect, workingSize, type PreviewScale, type Size } from "./layout";
import { Presenter } from "./present";
import { RefreshMeter } from "./refresh-meter";
import type { FromRenderer, RenderStats, ToRenderer } from "./protocol";

/** What the renderer needs from its worker scope. */
export interface RendererScope extends Endpoint {
  requestAnimationFrame(callback: (now: number) => void): number;
}

interface Gpu {
  canvas: OffscreenCanvas;
  renderer: THREE.WebGLRenderer;
  uploader: FrameUploader;
  presenter: Presenter;
  target: THREE.WebGLRenderTarget;
}

interface Clip {
  sequence: Size;
  rotation: number;
  duration: number;
}

const STATS_INTERVAL_MS = 1000;
/**
 * Playback start. Starting a hardware decoder briefly stalls the GPU, which delays display
 * refreshes by 30-100 ms; a clock already running then skips frames. So the clock waits until
 * the decoder has filled its frames-in-flight budget (it is then idle, waiting for credit) and
 * the last few refreshes arrived on time. MAX_START_WAIT_MS bounds the wait on a machine that
 * never looks steady.
 */
const PREROLL_FRAMES = DEFAULT_MAX_FRAMES_IN_FLIGHT;
const STEADY_REFRESHES = 3;
const MAX_START_WAIT_MS = 1000;
/**
 * For this long after the clock starts, a late refresh delays the clock instead of skipping
 * frames: the first uploads from a new decoder can still stall the GPU once or twice.
 */
const START_GRACE_MS = 500;

export function serveRenderer(scope: RendererScope): void {
  let gpu: Gpu | null = null;
  let decoder: DecodeClient | null = null;
  let clip: Clip | null = null;
  let canvasSize: Size = { width: 1, height: 1 };
  let scale: PreviewScale = 1;
  let contextLost = false;
  let contextRestores = 0;

  const queue = new FrameQueue<DecodedFrame>();
  let playing = false;
  let playbackStop: (() => void) | null = null;
  let decodeEnded = false;
  let playFrom = 0;
  let clockStart: number | null = null;
  let playhead = 0;
  /** Interval of the frame in the working target, if any. */
  let shown: { time: number; duration: number } | null = null;
  let dirty = false;

  let presented = 0;
  let dropped = 0;
  let late = 0;
  let presentedSinceReport = 0;
  let tickTotal = 0;
  let tickMax = 0;
  let ticks = 0;
  let lastReport = 0;
  let lastTick: number | null = null;
  let refreshMax = 0;
  const refreshes = new RefreshMeter();
  let steadyRefreshes = 0;
  let playRequestedAt = 0;
  let startLatency = 0;

  const post = (message: FromRenderer) => scope.postMessage(message, []);

  function init(canvas: OffscreenCanvas, port: MessagePort, size: Size): void {
    const renderer = new THREE.WebGLRenderer({
      canvas,
      antialias: false,
      alpha: false,
      depth: false,
      stencil: false,
      powerPreference: "high-performance",
    });
    renderer.setPixelRatio(1);
    renderer.autoClear = false;
    canvas.addEventListener("webglcontextlost", () => {
      contextLost = true;
    });
    canvas.addEventListener("webglcontextrestored", () => {
      contextLost = false;
      contextRestores++;
      // three.js re-creates its GPU objects on next use; the picture itself must be redrawn.
      repaint();
    });
    gpu = {
      canvas,
      renderer,
      uploader: new FrameUploader(renderer),
      presenter: new Presenter(renderer),
      target: makeTarget(2, 2),
    };
    resize(size);
    decoder = new DecodeClient(port);
    scope.requestAnimationFrame(tick);
    post({ type: "ready" });
  }

  function resize(size: Size): void {
    canvasSize = { width: Math.max(1, size.width), height: Math.max(1, size.height) };
    gpu?.renderer.setSize(canvasSize.width, canvasSize.height, false);
    dirty = true;
  }

  function rebuildTarget(): void {
    if (!gpu || !clip) return;
    const size = workingSize(clip.sequence, scale);
    gpu.target.dispose();
    gpu.target = makeTarget(size.width, size.height);
  }

  /** Redraws the current picture after the working target was lost or rebuilt. */
  function repaint(): void {
    if (!playing && shown) void showExact(shown.time);
  }

  function releaseQueued(): void {
    for (const frame of queue.clear()) frame.release();
  }

  function stopPlayback(): void {
    playing = false;
    playbackStop?.();
    playbackStop = null;
    releaseQueued();
  }

  /**
   * Uploads a frame into the working target and releases it at once. Returns false when it
   * could not be uploaded because the context is lost.
   */
  function show(frame: DecodedFrame): boolean {
    try {
      shown = { time: frame.time, duration: frame.duration };
      if (!gpu || !clip || contextLost) return false;
      gpu.uploader.upload(frame.frame, clip.rotation, gpu.target);
      dirty = true;
      return true;
    } finally {
      frame.release();
    }
  }

  async function showExact(time: number): Promise<number | null> {
    if (!decoder) return null;
    const frame = await decoder.seek(time, "exact");
    if (!frame) return null;
    show(frame);
    return frame.time;
  }

  function play(from: number): void {
    if (!decoder || !clip) return;
    stopPlayback();
    playing = true;
    decodeEnded = false;
    playFrom = from;
    playhead = from;
    clockStart = null;
    presented = dropped = late = 0;
    playRequestedAt = performance.now();
    startLatency = 0;
    const playback = decoder.play(from, clip.duration, (frame) => queue.push(frame));
    playbackStop = playback.stop;
    playback.done.then(
      () => (decodeEnded = true),
      (error: unknown) => post({ type: "error", message: String(error) }),
    );
  }

  function tick(now: number): void {
    const start = performance.now();
    const refresh = lastTick === null ? 0 : now - lastTick;
    lastTick = now;
    refreshMax = Math.max(refreshMax, refresh);
    refreshes.add(refresh);
    const lateRefresh = refreshes.isLate(refresh);
    steadyRefreshes = refreshes.period !== null && !lateRefresh ? steadyRefreshes + 1 : 0;
    if (playing) {
      if (clockStart === null) {
        const ready = queue.length >= PREROLL_FRAMES && steadyRefreshes >= STEADY_REFRESHES;
        const shortClip = decodeEnded && queue.length > 0;
        const waitedTooLong = queue.length > 0 && now - playRequestedAt > MAX_START_WAIT_MS;
        if (ready || shortClip || waitedTooLong) {
          clockStart = now;
          startLatency = now - playRequestedAt;
        }
      }
      if (clockStart !== null && now - clockStart < START_GRACE_MS && lateRefresh) {
        clockStart += refresh - (refreshes.period ?? refresh);
      }
      if (clockStart !== null) {
        playhead = playFrom + Math.round(((now - clockStart) * FLICKS_PER_SECOND) / 1000);
        const pick = queue.take(playhead);
        for (const frame of pick.dropped) frame.release();
        dropped += pick.dropped.length;
        if (pick.frame) {
          if (show(pick.frame)) {
            presented++;
            presentedSinceReport++;
          } else {
            dropped++;
          }
        } else if (shown && playhead >= shown.time + shown.duration && !decodeEnded) {
          late++;
        }
        if (decodeEnded && queue.length === 0 && clip && playhead >= clip.duration) {
          playing = false;
          playbackStop = null;
        }
      }
    }

    if (dirty && gpu && clip && !contextLost) {
      const picture = workingSize(clip.sequence, 1);
      gpu.presenter.present(gpu.target.texture, containRect(picture, canvasSize));
      dirty = false;
    }

    const elapsed = performance.now() - start;
    tickTotal += elapsed;
    tickMax = Math.max(tickMax, elapsed);
    ticks++;
    if (now - lastReport >= STATS_INTERVAL_MS) {
      report(now - lastReport);
      lastReport = now;
    }
    scope.requestAnimationFrame(tick);
  }

  function report(intervalMs: number): void {
    const stats: RenderStats = {
      fps: Math.round((presentedSinceReport * 10000) / intervalMs) / 10,
      presented,
      dropped,
      late,
      tickMeanMs: ticks ? Math.round((tickTotal / ticks) * 100) / 100 : 0,
      tickMaxMs: Math.round(tickMax * 100) / 100,
      refreshMaxMs: Math.round(refreshMax * 10) / 10,
      playhead,
      startLatencyMs: Math.round(startLatency),
      playing,
      contextLost,
      contextRestores,
      working: gpu
        ? { width: gpu.target.width, height: gpu.target.height }
        : { width: 0, height: 0 },
      framesOutstanding: decoder?.outstanding ?? 0,
      queued: queue.length,
    };
    post({ type: "stats", stats });
    presentedSinceReport = tickTotal = tickMax = ticks = refreshMax = 0;
  }

  async function handle(message: ToRenderer): Promise<void> {
    switch (message.type) {
      case "init":
        init(message.canvas, message.decoder, message.size);
        return;
      case "resize":
        resize(message.size);
        return;
      case "open": {
        if (!decoder) throw new Error("Renderer is not initialised");
        stopPlayback();
        shown = null;
        const result = await decoder.open(message.input);
        if (result.ok) {
          clip = {
            sequence: { width: result.width, height: result.height },
            rotation: result.rotation,
            duration: result.duration,
          };
          rebuildTarget();
          await showExact(0);
        }
        post({ type: "opened", id: message.id, result });
        return;
      }
      case "play":
        play(message.from);
        return;
      case "pause":
        stopPlayback();
        return;
      case "seek":
        stopPlayback();
        post({ type: "seeked", id: message.id, time: await showExact(message.time) });
        return;
      case "preview-scale":
        scale = message.scale;
        rebuildTarget();
        repaint();
        return;
      case "lose-context": {
        const gl = gpu?.renderer.getContext();
        const extension = gl?.getExtension("WEBGL_lose_context");
        if (!extension) throw new Error("WEBGL_lose_context is not available");
        extension.loseContext();
        setTimeout(() => extension.restoreContext(), message.restoreAfterMs);
        return;
      }
    }
  }

  scope.onmessage = (event: MessageEvent<ToRenderer>) => {
    const message = event.data;
    handle(message).catch((error: unknown) => {
      const text = error instanceof Error ? error.message : String(error);
      post(
        "id" in message
          ? { type: "error", id: message.id, message: text }
          : { type: "error", message: text },
      );
    });
  };
}
