// The render worker: owns the OffscreenCanvas and its WebGL2 context, plays a layered edit list
// from a pool of decode workers (frames arrive straight from them over MessagePorts), composites
// the layers and puts the result on screen.
//
// Clips with sound follow the audio clock: the playhead is the timeline time being heard when the
// next refresh reaches the screen. Otherwise playback runs on the display refresh. Either way the
// start waits until the decoders are primed and the display is steady (see PREROLL_FRAMES).
//
// Decoder scheduling: on every layer, the run at the playhead and the runs coming up
// (timeline.ts `wantedRuns`) get a decoder stream, current runs first and top layer first. When
// there are not enough decoders for every current run, the layers left over show keyframe-only
// updates from a reserved decoder instead of stalling the others.

import { ClockBuffer, heardSessionTime, PlaybackState } from "@renderly/audio";
import {
  DecodeClient,
  DEFAULT_MAX_FRAMES_IN_FLIGHT,
  FLICKS_PER_SECOND,
  type DecodedFrame,
  type Endpoint,
  type MediaInput,
  type OpenResult,
} from "@renderly/media";
import * as THREE from "three";
import { Compositor } from "./compositor";
import { FrameQueue } from "./frame-queue";
import { FrameUploader } from "./frame-upload";
import { containRect, workingSize, type PreviewScale, type Size } from "./layout";
import { decodedFrameBytes, MemoryBudget, mipmappedTextureBytes } from "./memory";
import { Presenter } from "./present";
import type { FromRenderer, LayerStats, LoadResult, RenderStats, ToRenderer } from "./protocol";
import { RefreshMeter } from "./refresh-meter";
import {
  runIdAt,
  sourceTime,
  timelineTime,
  toTimeline,
  wantedRuns,
  type Cut,
  type Run,
  type Timeline,
} from "./timeline";

/** What the renderer needs from its worker scope. */
export interface RendererScope extends Endpoint {
  requestAnimationFrame(callback: (now: number) => void): number;
}

interface Gpu {
  canvas: OffscreenCanvas;
  renderer: THREE.WebGLRenderer;
  uploader: FrameUploader;
  presenter: Presenter;
  compositor: Compositor;
}

type SourceInfo = Extract<OpenResult, { ok: true }>;

interface Slot {
  client: DecodeClient;
  /** Source the decoder has open, or -1. */
  source: number;
  stream: Stream | null;
  lastUsed: number;
}

/** A decoded frame placed on the timeline. */
interface TimelineFrame {
  decoded: DecodedFrame;
  run: number;
  time: number;
  duration: number;
}

interface Stream {
  run: number;
  slot: Slot;
  queue: FrameQueue<TimelineFrame>;
  ended: boolean;
  stopped: boolean;
  stop: () => void;
}

/** Per layer, indexed like `timeline.layers`. */
interface LayerState {
  currentRun: number;
  /** The frame in the layer's target, if any. */
  shown: { run: number; time: number; duration: number } | null;
  cutAt: number | null;
  presented: number;
  presentedSinceReport: number;
  dropped: number;
  late: number;
  keyframes: number;
  lastKeyframeAt: number;
}

const STATS_INTERVAL_MS = 1000;
/**
 * Playback start. Starting a hardware decoder briefly stalls the GPU, which delays display
 * refreshes by 30-100 ms; a clock already running then skips frames. So the clock waits until
 * the decoders have filled their frames-in-flight budget (they are then idle, waiting for credit)
 * and the last few refreshes arrived on time. MAX_START_WAIT_MS bounds the wait on a machine that
 * never looks steady.
 */
const PREROLL_FRAMES = DEFAULT_MAX_FRAMES_IN_FLIGHT;
const STEADY_REFRESHES = 3;
const MAX_START_WAIT_MS = 1000;
/**
 * For this long after the display clock starts, a late refresh delays the clock instead of
 * skipping frames: the first uploads from a new decoder can still stall the GPU once or twice.
 */
const START_GRACE_MS = 500;
/** Runs after the current one that always have a decoder ready, and how far ahead in time. */
const MIN_RUNS_AHEAD = 2;
const LOOKAHEAD = 1.5 * FLICKS_PER_SECOND;
/**
 * Runs per layer that may be prepared at once. More than there are decoders: a decoder is freed
 * as soon as its run is fully decoded (its frames wait in the render queue), so a string of clips
 * a few frames long is prepared one after another on whichever decoder frees up first.
 */
const MAX_PREPARED_RUNS = 8;
/**
 * Gap between decode submissions for streams prepared while the picture is playing. Feeding a
 * hardware decoder 4K frames in a burst stalls the GPU thread and drops frames on screen; 8 ms
 * per decoder does not (measured in S0.5, also with three decoders at once). Streams started
 * before the clock runs are not paced: nothing is moving on screen yet.
 */
const PREPARE_PACE_MS = 8;
/** How often a layer without a decoder is refreshed from its nearest keyframe. */
const KEYFRAME_INTERVAL_MS = 500;
/**
 * The renderer's GPU memory ceiling: render targets, the upload texture and decoded frames on
 * loan. Targets past it are refused; frames are bounded by the decoders' in-flight limit.
 */
export const GPU_MEMORY_CEILING = 1536 * 1024 * 1024;

const newLayerState = (): LayerState => ({
  currentRun: -1,
  shown: null,
  cutAt: null,
  presented: 0,
  presentedSinceReport: 0,
  dropped: 0,
  late: 0,
  keyframes: 0,
  lastKeyframeAt: -Infinity,
});

export function serveRenderer(scope: RendererScope): void {
  let gpu: Gpu | null = null;
  const budget = new MemoryBudget(GPU_MEMORY_CEILING);
  /** Decoders for streams; the last decoder in the pool is kept for keyframe updates and seeks. */
  let slots: Slot[] = [];
  let reserved: Slot | null = null;
  let sources: { input: MediaInput; info: SourceInfo | null }[] = [];
  let timeline: Timeline = { runs: [], layers: [], duration: 0 };
  let sequence: Size = { width: 2, height: 2 };
  let canvasSize: Size = { width: 1, height: 1 };
  let scale: PreviewScale = 1;
  let contextLost = false;
  let contextRestores = 0;
  let clock: ClockBuffer | null = null;

  const streams = new Map<number, Stream>();
  let layers: LayerState[] = [];
  let playing = false;
  let playFrom = 0;
  let clockStart: number | null = null;
  let playhead = 0;
  let dirty = false;
  let keyframeBusy = false;
  let exactChain: Promise<unknown> = Promise.resolve();
  /** Audio session this playback follows; 0 when it runs on the display refresh. */
  let audioSession = 0;
  let goSent = false;
  /** Time of the first frame this play put on the bottom layer, to check resume continuity. */
  let firstShown: number | null = null;

  // Stats since the last play started.
  let syncChecks = 0;
  let outOfSync = 0;
  let syncMinMs = Infinity;
  let syncMaxMs = -Infinity;
  let cutsPassed = 0;
  let cutsHeld = 0;
  let maxHoldMs = 0;
  let cutsInexact = 0;
  let droppedAtCuts = 0;
  let opens = 0;
  // Per report interval.
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
  const runOf = (id: number) => timeline.runs[id] as Run;

  function init(
    canvas: OffscreenCanvas,
    ports: MessagePort[],
    audioClock: SharedArrayBuffer,
    size: Size,
  ): void {
    clock = new ClockBuffer(audioClock);
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
      // three.js re-creates its GPU objects on next use; the pictures must be redrawn.
      repaint();
    });
    gpu = {
      canvas,
      renderer,
      uploader: new FrameUploader(renderer),
      presenter: new Presenter(renderer),
      compositor: new Compositor(renderer, budget),
    };
    resize(size);
    const pool = ports.map((port) => ({
      client: new DecodeClient(port),
      source: -1,
      stream: null,
      lastUsed: 0,
    }));
    reserved = pool.pop() ?? null;
    slots = pool;
    scope.requestAnimationFrame(tick);
    post({ type: "ready" });
  }

  function resize(size: Size): void {
    canvasSize = { width: Math.max(1, size.width), height: Math.max(1, size.height) };
    gpu?.renderer.setSize(canvasSize.width, canvasSize.height, false);
    dirty = true;
  }

  function rebuildTargets(): void {
    gpu?.compositor.resize(workingSize(sequence, scale));
    for (const state of layers) state.shown = null;
  }

  /** Redraws the pictures after the targets were lost or rebuilt. */
  function repaint(): void {
    if (!playing) void showExact(playhead);
  }

  // --- decoders ---

  async function openOn(slot: Slot, source: number): Promise<OpenResult> {
    const entry = sources[source];
    if (!entry) throw new Error(`No source ${source}`);
    const result = await slot.client.open(entry.input);
    slot.source = result.ok ? source : -1;
    opens++;
    return result;
  }

  /** An idle stream decoder, preferring one that already has `source` open. */
  function idleSlot(source: number): Slot | null {
    const idle = slots.filter((slot) => !slot.stream);
    return (
      idle.find((slot) => slot.source === source) ??
      idle.sort((a, b) => a.lastUsed - b.lastUsed)[0] ??
      null
    );
  }

  /**
   * Starts a decoder stream for run `id` if a decoder is free. It decodes from timeline time `t`
   * when that falls inside the run (playback starting mid-run), else from the run's start.
   */
  function ensureStream(id: number, t: number): void {
    const run = timeline.runs[id];
    if (!run || streams.has(id)) return;
    const slot = idleSlot(run.source);
    if (!slot) return;
    const stream: Stream = {
      run: id,
      slot,
      queue: new FrameQueue<TimelineFrame>(),
      ended: false,
      stopped: false,
      stop: () => {},
    };
    slot.stream = stream;
    slot.lastUsed = performance.now();
    streams.set(id, stream);
    const from = t > run.start ? sourceTime(run, t) : run.in;
    // Unpaced only for runs under the playhead before the clock starts: nothing moves yet.
    const atPlayhead = t >= run.start && t < run.end;
    const paceMs = clockStart === null && atPlayhead ? 0 : PREPARE_PACE_MS;
    startStream(stream, run, from, paceMs).catch((error: unknown) => {
      post({ type: "error", message: `Run ${id}: ${String(error)}` });
      retire(stream);
    });
  }

  async function startStream(stream: Stream, run: Run, from: number, paceMs: number) {
    const { slot } = stream;
    if (slot.source !== run.source) {
      const result = await openOn(slot, run.source);
      if (!result.ok) throw new Error(`Cannot open source ${run.source}`);
    }
    if (stream.stopped) return;
    const playback = slot.client.play(
      from,
      run.out,
      (decoded) => {
        if (stream.stopped) {
          decoded.release();
          return;
        }
        stream.queue.push({
          decoded,
          run: stream.run,
          time: timelineTime(run, decoded.time),
          duration: decoded.duration,
        });
      },
      paceMs,
    );
    stream.stop = playback.stop;
    await playback.done;
    stream.ended = true;
    // Everything is decoded and queued here: the decoder can prepare another run.
    if (slot.stream === stream) slot.stream = null;
  }

  /** Stops a stream, frees its decoder and releases what it still holds. */
  function retire(stream: Stream): number {
    if (stream.stopped) return 0;
    stream.stopped = true;
    stream.stop();
    const left = stream.queue.clear();
    for (const frame of left) frame.decoded.release();
    if (stream.slot.stream === stream) stream.slot.stream = null;
    streams.delete(stream.run);
    return left.length;
  }

  function retireAll(): void {
    for (const stream of [...streams.values()]) retire(stream);
  }

  // --- pictures ---

  /**
   * Uploads a frame into its layer's target and releases it at once. Returns false when it could
   * not be uploaded (context lost, or no target under the memory ceiling).
   */
  function show(
    layerIndex: number,
    decoded: DecodedFrame,
    run: number,
    time: number,
    duration: number,
  ) {
    try {
      const state = layers[layerIndex];
      if (!state) return false;
      state.shown = { run, time, duration };
      const runInfo = timeline.runs[run];
      const info = sources[runInfo?.source ?? -1]?.info;
      const layer = timeline.layers[layerIndex]?.layer ?? 0;
      const target = gpu?.compositor.layerTarget(layer);
      if (!gpu || !info || !target || contextLost) return false;
      gpu.uploader.upload(decoded.frame, info.rotation, target);
      budget.set(
        "upload",
        mipmappedTextureBytes(decoded.frame.codedWidth, decoded.frame.codedHeight),
      );
      return true;
    } finally {
      decoded.release();
    }
  }

  /** Composites every layer that has a run at the playhead, with that run's transform. */
  function composite(): void {
    if (!gpu || contextLost) return;
    const draws = timeline.layers.flatMap(({ layer }, i) => {
      const state = layers[i];
      if (!state || state.currentRun < 0 || !state.shown) return [];
      return [{ layer, transform: runOf(state.currentRun).transform }];
    });
    gpu.compositor.composite(draws);
    dirty = true;
  }

  /** Shows the exact frames at timeline time `t`. Resolves with the bottom layer's frame time. */
  function showExact(t: number): Promise<number | null> {
    // One at a time: two running together seek the same decoder, and the newer seek cancels the
    // older one, leaving a layer undrawn (seen in the S0.8 parity test).
    const next = exactChain.then(() => showExactNow(t));
    exactChain = next.catch(() => null);
    return next;
  }

  async function showExactNow(t: number): Promise<number | null> {
    let bottom: number | null = null;
    for (const [i] of timeline.layers.entries()) {
      const state = layers[i] as LayerState;
      const id = runIdAt(timeline, i, t);
      state.currentRun = id;
      if (id < 0 || !reserved) continue;
      const run = runOf(id);
      if (reserved.source !== run.source && !(await openOn(reserved, run.source)).ok) continue;
      const decoded = await reserved.client.seek(sourceTime(run, t), "exact");
      if (!decoded) continue;
      const time = timelineTime(run, decoded.time);
      show(i, decoded, id, time, decoded.duration);
      if (i === 0) bottom = time;
    }
    composite();
    return bottom;
  }

  /** Refreshes a layer that has no decoder from the keyframe at or before `t`. */
  function keyframeUpdate(layerIndex: number, id: number, t: number, now: number): void {
    const state = layers[layerIndex];
    if (!state || !reserved || keyframeBusy || now - state.lastKeyframeAt < KEYFRAME_INTERVAL_MS) {
      return;
    }
    keyframeBusy = true;
    state.lastKeyframeAt = now;
    const slot = reserved;
    const run = runOf(id);
    void (async () => {
      try {
        if (slot.source !== run.source && !(await openOn(slot, run.source)).ok) return;
        const decoded = await slot.client.seek(sourceTime(run, t), "keyframe");
        if (!decoded) return;
        if (!playing || state.currentRun !== id) {
          decoded.release();
          return;
        }
        if (show(layerIndex, decoded, id, timelineTime(run, decoded.time), decoded.duration)) {
          state.keyframes++;
          composite();
        }
      } finally {
        keyframeBusy = false;
      }
    })();
  }

  // --- playback ---

  function stopPlayback(): void {
    playing = false;
    retireAll();
  }

  function play(from: number, session: number): void {
    if (timeline.runs.length === 0) return;
    stopPlayback();
    playing = true;
    audioSession = session;
    goSent = false;
    firstShown = null;
    playFrom = from;
    playhead = from;
    clockStart = null;
    layers = timeline.layers.map((_, i) => {
      const state = newLayerState();
      state.currentRun = runIdAt(timeline, i, from);
      state.shown = layers[i]?.shown ?? null;
      return state;
    });
    syncChecks = outOfSync = 0;
    syncMinMs = Infinity;
    syncMaxMs = -Infinity;
    cutsPassed = cutsHeld = cutsInexact = droppedAtCuts = opens = 0;
    maxHoldMs = 0;
    budget.resetPeak();
    if (gpu) gpu.compositor.refused = 0;
    playRequestedAt = performance.now();
    startLatency = 0;
    schedule(from);
  }

  /**
   * Keeps decoder streams on the runs `wantedRuns` asks for, current runs first, and retires the
   * rest. Decoders are limited; runs left without one fall back to keyframe updates.
   */
  function schedule(t: number): void {
    const { current, ahead } = wantedRuns(timeline, t, {
      minAhead: MIN_RUNS_AHEAD,
      lookahead: LOOKAHEAD,
      maxStreams: MAX_PREPARED_RUNS,
    });
    const wanted = new Set([...current, ...ahead]);
    for (const stream of [...streams.values()]) {
      if (!wanted.has(stream.run)) {
        const left = retire(stream);
        if (runOf(stream.run).end <= t) droppedAtCuts += left;
      }
    }
    for (const id of [...current, ...ahead]) ensureStream(id, t);
  }

  /** True when every current run that has a stream is primed. */
  function primed(): boolean {
    let any = false;
    for (const state of layers) {
      const stream = streams.get(state.currentRun);
      if (!stream) continue;
      any = true;
      const ready =
        stream.queue.length >= PREROLL_FRAMES || (stream.ended && stream.queue.length > 0);
      if (!ready) return false;
    }
    return any;
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
      const isPrimed = clockStart === null && primed();
      const ready =
        isPrimed &&
        (steadyRefreshes >= STEADY_REFRESHES || now - playRequestedAt > MAX_START_WAIT_MS);
      const time = audioSession
        ? audioTime(now, ready)
        : refreshTime(now, ready, refresh, lateRefresh);
      if (time !== null) advance(time, now);
    }

    if (dirty && gpu && !contextLost) {
      const texture = gpu.compositor.texture;
      if (texture) gpu.presenter.present(texture, containRect(sequence, canvasSize));
      dirty = false;
    }

    measureMemory();
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

  /** Moves playback to timeline time `time`: schedules decoders and shows each layer's frame. */
  function advance(time: number, now: number): void {
    playhead = time;
    if (playhead >= timeline.duration) {
      stopPlayback();
      return;
    }
    schedule(playhead);
    let changed = false;
    for (const [i] of timeline.layers.entries()) {
      const state = layers[i] as LayerState;
      const id = runIdAt(timeline, i, playhead);
      if (id !== state.currentRun) {
        if (id >= 0) {
          cutsPassed++;
          state.cutAt = now;
        }
        state.currentRun = id;
        changed = true; // a layer starts, ends or changes transform
      }
      if (id < 0) continue;
      const stream = streams.get(id);
      if (!stream) {
        keyframeUpdate(i, id, playhead, now);
        continue;
      }
      const pick = stream.queue.take(playhead);
      for (const frame of pick.dropped) frame.decoded.release();
      state.dropped += pick.dropped.length;
      if (state.cutAt !== null) droppedAtCuts += pick.dropped.length;
      const frame = pick.frame;
      if (frame) {
        if (state.cutAt !== null) firstFrameOfCut(state, frame, now);
        if (show(i, frame.decoded, frame.run, frame.time, frame.duration)) {
          if (i === 0) firstShown ??= frame.time;
          state.presented++;
          state.presentedSinceReport++;
          changed = true;
        } else {
          state.dropped++;
        }
      } else if (
        !state.shown ||
        state.shown.run !== id ||
        playhead >= state.shown.time + state.shown.duration
      ) {
        if (!stream.ended || stream.queue.length > 0) state.late++;
      }
    }
    if (changed) composite();
    if (audioSession) checkSync();
  }

  /** Records hold time and exactness for the first frame shown after a cut. */
  function firstFrameOfCut(state: LayerState, frame: TimelineFrame, now: number): void {
    const holdMs = now - (state.cutAt ?? now);
    if (holdMs > 0) {
      cutsHeld++;
      maxHoldMs = Math.max(maxHoldMs, holdMs);
    }
    // The first frame after a cut must be the one covering the playhead.
    if (!(frame.time <= playhead && playhead < frame.time + frame.duration)) cutsInexact++;
    state.cutAt = null;
  }

  /** Display-refresh clock, for clips without sound. */
  function refreshTime(now: number, ready: boolean, refresh: number, lateRefresh: boolean) {
    if (clockStart === null && ready) {
      clockStart = now;
      startLatency = now - playRequestedAt;
    }
    if (clockStart === null) return null;
    if (now - clockStart < START_GRACE_MS && lateRefresh) {
      clockStart += refresh - (refreshes.period ?? refresh);
    }
    return playFrom + Math.round(((now - clockStart) * FLICKS_PER_SECOND) / 1000);
  }

  /**
   * Audio clock: the session time heard when this refresh reaches the screen (assumed to be one
   * refresh period from now). Before the picture is ready the audio is held with the go flag.
   */
  function audioTime(now: number, ready: boolean) {
    if (!clock) return null;
    if (!goSent && ready) {
      clock.setGo(audioSession);
      goSent = true;
    }
    const reading = clock.read();
    const mapping = clock.readMapping();
    if (
      !reading ||
      !mapping ||
      reading.session !== audioSession ||
      (reading.state !== PlaybackState.Playing && reading.state !== PlaybackState.Stopping)
    ) {
      return null;
    }
    const displayAt = performance.timeOrigin + now + (refreshes.period ?? 0);
    const heard = heardSessionTime(reading, mapping, displayAt);
    if (heard === null) return null;
    if (clockStart === null) {
      clockStart = now;
      startLatency = now - playRequestedAt;
    }
    return playFrom + heard;
  }

  /**
   * Records how far the heard time is into the bottom layer's frame (the layer the sound comes
   * from). In sync: 0 <= offset < duration.
   */
  function checkSync(): void {
    const shown = layers[0]?.shown;
    if (!shown || layers[0]?.currentRun !== shown.run) return;
    // A layer on keyframe-only updates lags by design; sync is measured on decoded playback.
    if (!streams.has(shown.run)) return;
    const offset = playhead - shown.time;
    const ms = (offset * 1000) / FLICKS_PER_SECOND;
    syncChecks++;
    if (offset < 0 || offset >= shown.duration) outOfSync++;
    syncMinMs = Math.min(syncMinMs, ms);
    syncMaxMs = Math.max(syncMaxMs, ms);
  }

  /** Accounts for decoded frames on loan; render targets account for themselves. */
  function measureMemory(): void {
    let frames = 0;
    for (const slot of [...slots, ...(reserved ? [reserved] : [])]) {
      const info = sources[slot.source]?.info;
      if (info) frames += slot.client.outstanding * decodedFrameBytes(info.width, info.height);
    }
    budget.set("frames", frames);
  }

  function report(intervalMs: number): void {
    const perLayer: LayerStats[] = timeline.layers.map(({ layer }, i) => {
      const state = layers[i] ?? newLayerState();
      return {
        layer,
        fps: Math.round((state.presentedSinceReport * 10000) / intervalMs) / 10,
        presented: state.presented,
        dropped: state.dropped,
        late: state.late,
        keyframes: state.keyframes,
        decoding: streams.has(state.currentRun),
      };
    });
    const active = perLayer.filter((_, i) => (layers[i]?.currentRun ?? -1) >= 0);
    const sum = (key: "presented" | "dropped" | "late") =>
      perLayer.reduce((n, layer) => n + layer[key], 0);
    const mb = (bytes: number) => Math.round(bytes / 1048576);
    const stats: RenderStats = {
      fps: active.length ? Math.min(...active.map((layer) => layer.fps)) : 0,
      presented: sum("presented"),
      dropped: sum("dropped"),
      late: sum("late"),
      tickMeanMs: ticks ? Math.round((tickTotal / ticks) * 100) / 100 : 0,
      tickMaxMs: Math.round(tickMax * 100) / 100,
      refreshMaxMs: Math.round(refreshMax * 10) / 10,
      playhead,
      startLatencyMs: Math.round(startLatency),
      playing,
      contextLost,
      contextRestores,
      working: workingSize(sequence, scale),
      framesOutstanding: [...slots, ...(reserved ? [reserved] : [])].reduce(
        (n, slot) => n + slot.client.outstanding,
        0,
      ),
      queued: [...streams.values()].reduce((n, stream) => n + stream.queue.length, 0),
      audioClock: audioSession !== 0,
      firstShown,
      sync: {
        checks: syncChecks,
        outOfSync,
        minMs: syncChecks ? Math.round(syncMinMs * 10) / 10 : 0,
        maxMs: syncChecks ? Math.round(syncMaxMs * 10) / 10 : 0,
      },
      cuts: {
        passed: cutsPassed,
        held: cutsHeld,
        maxHoldMs: Math.round(maxHoldMs * 10) / 10,
        inexact: cutsInexact,
        droppedAtCuts,
      },
      decoders: {
        pool: slots.length + (reserved ? 1 : 0),
        busy: slots.filter((slot) => slot.stream).length,
        opens,
      },
      layers: perLayer,
      memory: {
        usedMB: mb(budget.used),
        peakMB: mb(budget.peak),
        ceilingMB: mb(budget.ceiling),
        refused: gpu?.compositor.refused ?? 0,
      },
    };
    post({ type: "stats", stats });
    for (const state of layers) state.presentedSinceReport = 0;
    tickTotal = tickMax = ticks = refreshMax = 0;
  }

  // --- loading ---

  async function load(inputs: MediaInput[], cuts: Cut[] | null): Promise<LoadResult> {
    if (!reserved) throw new Error("Renderer is not initialised");
    stopPlayback();
    timeline = { runs: [], layers: [], duration: 0 };
    layers = [];
    sources = inputs.map((input) => ({ input, info: null }));
    // Open each source once to learn its size and duration, spreading them over the decoders.
    const pool = [...slots, reserved];
    const results: OpenResult[] = [];
    for (const [i, entry] of sources.entries()) {
      const result = await openOn(pool[i % pool.length] as Slot, i);
      results.push(result);
      if (result.ok) entry.info = result;
    }
    const first = results[0];
    if (!first?.ok || results.some((result) => !result.ok)) {
      return { sources: results, cuts: [], duration: 0 };
    }
    const resolved = cuts ?? [{ source: 0, in: 0, out: first.duration, start: 0 }];
    timeline = toTimeline(resolved);
    layers = timeline.layers.map(() => newLayerState());
    const bottom = timeline.runs[timeline.layers[0]?.ids[0] ?? 0];
    const opening = sources[bottom?.source ?? 0]?.info ?? first;
    sequence = { width: opening.width, height: opening.height };
    rebuildTargets();
    playhead = bottom?.start ?? 0;
    await showExact(playhead);
    return { sources: results, cuts: resolved, duration: timeline.duration };
  }

  async function handle(message: ToRenderer): Promise<void> {
    switch (message.type) {
      case "init":
        init(message.canvas, message.decoders, message.audioClock, message.size);
        return;
      case "resize":
        resize(message.size);
        return;
      case "load":
        post({ type: "loaded", id: message.id, result: await load(message.sources, message.cuts) });
        return;
      case "play":
        play(message.from, message.session);
        return;
      case "pause": {
        stopPlayback();
        // A resting playhead is frame-exact: the start of the bottom layer's frame on screen.
        // Resuming from there shows that same frame first.
        const shown = layers[0]?.shown;
        if (shown && layers[0]?.currentRun === shown.run) playhead = shown.time;
        post({ type: "paused", id: message.id, time: playhead });
        return;
      }
      case "read-frame": {
        stopPlayback();
        playhead = message.time;
        await showExact(message.time);
        const pixels = gpu?.compositor.readPixels();
        if (!pixels) throw new Error("No composite to read");
        const { width, height } = workingSize(sequence, scale);
        scope.postMessage({ type: "frame-pixels", id: message.id, pixels, width, height }, [
          pixels.buffer,
        ]);
        return;
      }
      case "seek":
        stopPlayback();
        playhead = message.time;
        post({ type: "seeked", id: message.id, time: await showExact(message.time) });
        return;
      case "preview-scale":
        scale = message.scale;
        rebuildTargets();
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
