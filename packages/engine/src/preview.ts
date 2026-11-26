// Main-thread side of the preview: starts a pool of decode workers, a render worker and the audio
// output, connects the decoders to the renderer with MessageChannels and hands the canvas and the
// audio clock to the renderer. After that the main thread only forwards commands, receives a stats
// message once a second, and lets the audio output refresh its timing mapping.

import { createAudioOutput, type AudioOpenResult, type AudioOutput } from "@renderly/audio";
import { startDecodeWorker, type Flicks, type MediaInput, type OpenResult } from "@renderly/media";
import type { PreviewScale, Size, ViewerView } from "./layout";
import type { FromRenderer, LoadResult, RenderStats, ToRenderer } from "./protocol";
import type { Cut } from "./timeline";

export interface OpenedMedia {
  video: OpenResult;
  /** When this is not ok, playback runs on the display refresh, silently. */
  audio: AudioOpenResult;
}

export interface LoadedTimeline extends LoadResult {
  /** Audio result per source. Playback follows the audio clock when any source has audio. */
  audio: AudioOpenResult[];
}

/**
 * Decode workers in the pool: five for streams (current runs on every layer and the runs coming
 * up) and one kept for keyframe-only updates and seeks.
 */
export const DECODER_POOL_SIZE = 6;

export interface Preview {
  /** Opens one file as a timeline of one cut. */
  open(input: MediaInput): Promise<OpenedMedia>;
  /** Opens several files and plays `cuts` over them, at `size` (default: the first file's). */
  load(sources: MediaInput[], cuts: Cut[], size?: Size): Promise<LoadedTimeline>;
  /**
   * Changes the edit list in place, while playing too: `added` files are appended to the loaded
   * ones and `cuts` refer to all of them. Decoders and unchanged runs carry over. When the sound
   * (layer 0) changed during playback, playback restarts at the playhead so picture and sound
   * stay in sync.
   */
  update(added: MediaInput[], cuts: Cut[], size: Size): Promise<LoadedTimeline>;
  /** Fitted (zoom null) or zoomed and panned; `background` (0xRRGGBB) fills around the picture. */
  setView(view: ViewerView, background: number): void;
  /** The playhead and whether it is playing, as of the renderer's last refresh. Cheap. */
  readPlayhead(): { time: Flicks; playing: boolean; refreshes: number };
  play(from: Flicks): void;
  /** Stops; resolves with the playhead, where `play` should resume. */
  pause(): Promise<Flicks>;
  /** Shows the exact frame at `time`; resolves with the frame's start time, or null. */
  seek(time: Flicks): Promise<Flicks | null>;
  /** Parity test: the exact frame at `time`, composited, as 8-bit RGBA (rows bottom first). */
  readFrame(time: Flicks): Promise<Uint8Array>;
  /** Dragging the playhead: call on every move. Keyframe pictures while moving, exact at rest. */
  scrub(time: Flicks): void;
  /** The drag ended: resolves with the time of the exact frame on screen. */
  scrubEnd(): Promise<Flicks>;
  setPreviewScale(scale: PreviewScale): void;
  /** Test hook: loses the WebGL context and restores it after `restoreAfterMs`. */
  loseContext(restoreAfterMs: number): void;
  onStats(listener: (stats: RenderStats) => void): void;
  onError(listener: (message: string) => void): void;
  readonly audio: AudioOutput;
  dispose(): void;
}

/** Canvas size in device pixels, exact where the browser reports it. */
function devicePixelSize(entry: ResizeObserverEntry): Size {
  const box = entry.devicePixelContentBoxSize?.[0];
  if (box) return { width: box.inlineSize, height: box.blockSize };
  const ratio = globalThis.devicePixelRatio || 1;
  return {
    width: Math.round(entry.contentRect.width * ratio),
    height: Math.round(entry.contentRect.height * ratio),
  };
}

type Reply = Extract<FromRenderer, { id: number }>;

/**
 * Tests only: a `slow-seek=<ms>` query parameter on the page makes every decoder seek wait that
 * long, so races that need a slow machine show up on a fast one (`pnpm verify` uses it).
 */
export function testSeekDelayMs(): number {
  const value = new URLSearchParams(globalThis.location?.search ?? "").get("slow-seek");
  const ms = Number(value);
  return value !== null && Number.isFinite(ms) && ms > 0 ? ms : 0;
}

export async function createPreview(canvas: HTMLCanvasElement): Promise<Preview> {
  const audio = await createAudioOutput();
  const decoders = Array.from({ length: DECODER_POOL_SIZE }, () => startDecodeWorker());
  const render = new Worker(new URL("./render-worker.ts", import.meta.url), {
    type: "module",
    name: "render",
  });
  const send = (message: ToRenderer, transfer: Transferable[] = []) =>
    render.postMessage(message, transfer);

  let nextId = 1;
  let session = 0;
  let hasAudio = false;
  let cuts: Cut[] = [];
  let duration = 0;
  let inputs: MediaInput[] = [];
  let sound: AudioOpenResult[] = [];
  const playheadBuffer = new SharedArrayBuffer(3 * Float64Array.BYTES_PER_ELEMENT);
  const playheadView = new Float64Array(playheadBuffer);
  const pending = new Map<number, { resolve: (m: Reply) => void; reject: (e: Error) => void }>();
  const statsListeners: ((stats: RenderStats) => void)[] = [];
  const errorListeners: ((message: string) => void)[] = [];
  const reportError = (message: string) => {
    for (const listener of errorListeners) listener(message);
  };

  render.onmessage = (event: MessageEvent<FromRenderer>) => {
    const message = event.data;
    switch (message.type) {
      case "stats":
        for (const listener of statsListeners) listener(message.stats);
        return;
      case "error":
        if (message.id !== undefined && pending.has(message.id)) {
          pending.get(message.id)?.reject(new Error(message.message));
          pending.delete(message.id);
          return;
        }
        reportError(message.message);
        return;
      case "loaded":
      case "seeked":
      case "paused":
      case "frame-pixels":
      case "scrubbed":
        pending.get(message.id)?.resolve(message);
        pending.delete(message.id);
        return;
      case "ready":
        return;
    }
  };

  const ratio = globalThis.devicePixelRatio || 1;
  const offscreen = canvas.transferControlToOffscreen();
  send(
    {
      type: "init",
      canvas: offscreen,
      decoders: decoders.map((decoder) => decoder.port),
      audioClock: audio.clock,
      playhead: playheadBuffer,
      testSeekDelayMs: testSeekDelayMs(),
      size: {
        width: Math.round(canvas.clientWidth * ratio),
        height: Math.round(canvas.clientHeight * ratio),
      },
    },
    [offscreen, ...decoders.map((decoder) => decoder.port)],
  );

  const observer = new ResizeObserver(([entry]) => {
    if (entry) send({ type: "resize", size: devicePixelSize(entry) });
  });
  try {
    observer.observe(canvas, { box: "device-pixel-content-box" });
  } catch {
    observer.observe(canvas);
  }

  function request<T extends Reply>(build: (id: number) => ToRenderer): Promise<T> {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve: (message) => resolve(message as T), reject });
      send(build(id));
    });
  }

  const loadAudio = (sources: MediaInput[]) =>
    audio.load(sources).catch((error: unknown): AudioOpenResult[] => {
      reportError(`Audio: ${String(error)}`);
      return sources.map(() => ({ ok: false, reason: "unsupported-format" }));
    });

  /** Sound comes from layer 0's unmuted cuts until the audio engine mixes layers (M2). */
  const soundCuts = (list: readonly Cut[]) =>
    list.filter((cut) => (cut.layer ?? 0) === 0 && !cut.muted);

  function setTimeline(list: Cut[], end: number): void {
    cuts = list;
    duration = end;
    hasAudio = soundCuts(list).some((cut) => sound[cut.source]?.ok === true);
  }

  async function loadTimeline(
    sources: MediaInput[],
    list: Cut[] | null,
    size?: Size,
  ): Promise<LoadedTimeline> {
    audio.stop();
    const [video, opened] = await Promise.all([
      request<Extract<Reply, { type: "loaded" }>>((id) => ({
        type: "load",
        id,
        sources,
        cuts: list,
        ...(size ? { size } : {}),
      })),
      loadAudio(sources),
    ]);
    inputs = [...sources];
    sound = opened;
    setTimeline(video.result.cuts, video.result.duration);
    return { ...video.result, audio: opened };
  }

  function startSound(from: Flicks): void {
    session++;
    if (hasAudio) {
      // The sound runs to the end of the timeline, silent past the last sounding cut, so the clock
      // it drives keeps going while upper layers still play.
      const list = soundCuts(cuts);
      const end = list.reduce((t, cut) => Math.max(t, cut.start + cut.out - cut.in), 0);
      const pad = duration > end ? [{ source: -1, in: 0, out: duration - end, start: end }] : [];
      audio.play(session, from, [...list, ...pad]);
    }
    send({ type: "play", from, session: hasAudio ? session : 0 });
  }

  return {
    audio,
    async load(sources, list, size) {
      return loadTimeline(sources, list, size);
    },
    async update(added, list, size) {
      const before = JSON.stringify(soundCuts(cuts));
      const wasPlaying = playheadView[1] === 1;
      if (added.length > 0) {
        inputs = [...inputs, ...added];
        if (wasPlaying) audio.stop();
        sound = await loadAudio(inputs);
      }
      const reply = await request<Extract<Reply, { type: "loaded" }>>((id) => ({
        type: "update",
        id,
        added,
        cuts: list,
        size,
      }));
      setTimeline(reply.result.cuts, reply.result.duration);
      const soundChanged = added.length > 0 || JSON.stringify(soundCuts(cuts)) !== before;
      if (wasPlaying && soundChanged && playheadView[1] === 1) startSound(playheadView[0] ?? 0);
      return { ...reply.result, audio: sound.slice(inputs.length - added.length) };
    },
    setView: (view, background) => send({ type: "view", view, background }),
    readPlayhead: () => ({
      time: playheadView[0] ?? 0,
      playing: playheadView[1] === 1,
      refreshes: playheadView[2] ?? 0,
    }),
    async open(input) {
      const loaded = await loadTimeline([input], null);
      return {
        video: loaded.sources[0] ?? {
          ok: false,
          reason: "unsupported-format",
          detail: "Nothing loaded",
        },
        audio: loaded.audio[0] ?? { ok: false, reason: "no-audio-track" },
      };
    },
    play(from) {
      startSound(from);
    },
    async pause() {
      audio.stop();
      const reply = await request<Extract<Reply, { type: "paused" }>>((id) => ({
        type: "pause",
        id,
      }));
      return reply.time;
    },
    async seek(time) {
      audio.stop();
      const reply = await request<Extract<Reply, { type: "seeked" }>>((id) => ({
        type: "seek",
        id,
        time,
      }));
      return reply.time;
    },
    async readFrame(time) {
      const reply = await request<Extract<Reply, { type: "frame-pixels" }>>((id) => ({
        type: "read-frame",
        id,
        time,
      }));
      return reply.pixels;
    },
    scrub(time) {
      audio.stop();
      send({ type: "scrub", time });
    },
    async scrubEnd() {
      const reply = await request<Extract<Reply, { type: "scrubbed" }>>((id) => ({
        type: "scrub-end",
        id,
      }));
      return reply.time;
    },
    setPreviewScale: (scale) => send({ type: "preview-scale", scale }),
    loseContext: (restoreAfterMs) => send({ type: "lose-context", restoreAfterMs }),
    onStats: (listener) => void statsListeners.push(listener),
    onError: (listener) => void errorListeners.push(listener),
    dispose() {
      observer.disconnect();
      render.terminate();
      for (const decoder of decoders) decoder.terminate();
      audio.dispose();
    },
  };
}
