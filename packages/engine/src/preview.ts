// Main-thread side of the preview: starts a pool of decode workers, a render worker and the audio
// output, connects the decoders to the renderer with MessageChannels and hands the canvas and the
// audio clock to the renderer. After that the main thread only forwards commands, receives a stats
// message once a second, and lets the audio output refresh its timing mapping.

import { createAudioOutput, type AudioOpenResult, type AudioOutput } from "@renderly/audio";
import { startDecodeWorker, type Flicks, type MediaInput, type OpenResult } from "@renderly/media";
import type { PreviewScale, Size } from "./layout";
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

/** Decode workers in the pool: the current run, two ahead, and one spare for short clips. */
export const DECODER_POOL_SIZE = 4;

export interface Preview {
  /** Opens one file as a timeline of one cut. */
  open(input: MediaInput): Promise<OpenedMedia>;
  /** Opens several files and plays `cuts` over them. */
  load(sources: MediaInput[], cuts: Cut[]): Promise<LoadedTimeline>;
  play(from: Flicks): void;
  /** Stops; resolves with the playhead, where `play` should resume. */
  pause(): Promise<Flicks>;
  /** Shows the exact frame at `time`; resolves with the frame's start time, or null. */
  seek(time: Flicks): Promise<Flicks | null>;
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

  async function loadTimeline(sources: MediaInput[], list: Cut[] | null): Promise<LoadedTimeline> {
    audio.stop();
    const [video, sound] = await Promise.all([
      request<Extract<Reply, { type: "loaded" }>>((id) => ({
        type: "load",
        id,
        sources,
        cuts: list,
      })),
      audio.load(sources).catch((error: unknown): AudioOpenResult[] => {
        reportError(`Audio: ${String(error)}`);
        return sources.map(() => ({ ok: false, reason: "unsupported-format" }));
      }),
    ]);
    cuts = video.result.cuts;
    hasAudio = cuts.length > 0 && sound.some((result) => result.ok);
    return { ...video.result, audio: sound };
  }

  return {
    audio,
    async load(sources, list) {
      return loadTimeline(sources, list);
    },
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
      session++;
      if (hasAudio) audio.play(session, from, cuts);
      send({ type: "play", from, session: hasAudio ? session : 0 });
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
