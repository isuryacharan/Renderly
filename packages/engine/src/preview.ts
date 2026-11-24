// Main-thread side of the preview: starts a decode worker, a render worker and the audio output,
// connects the decoder to the renderer with a MessageChannel and hands the canvas and the audio
// clock to the renderer. After that the main thread only forwards commands, receives a stats
// message once a second, and lets the audio output refresh its timing mapping.

import { createAudioOutput, type AudioOpenResult, type AudioOutput } from "@renderly/audio";
import { startDecodeWorker, type Flicks, type MediaInput, type OpenResult } from "@renderly/media";
import type { PreviewScale, Size } from "./layout";
import type { FromRenderer, RenderStats, ToRenderer } from "./protocol";

export interface OpenedMedia {
  video: OpenResult;
  /** When this is not ok, playback runs on the display refresh, silently. */
  audio: AudioOpenResult;
}

export interface Preview {
  open(input: MediaInput): Promise<OpenedMedia>;
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
  const decode = startDecodeWorker();
  const render = new Worker(new URL("./render-worker.ts", import.meta.url), {
    type: "module",
    name: "render",
  });
  const send = (message: ToRenderer, transfer: Transferable[] = []) =>
    render.postMessage(message, transfer);

  let nextId = 1;
  let session = 0;
  let hasAudio = false;
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
      case "opened":
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
      decoder: decode.port,
      audioClock: audio.clock,
      size: {
        width: Math.round(canvas.clientWidth * ratio),
        height: Math.round(canvas.clientHeight * ratio),
      },
    },
    [offscreen, decode.port],
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

  return {
    audio,
    async open(input) {
      audio.stop();
      const [video, sound] = await Promise.all([
        request<Extract<Reply, { type: "opened" }>>((id) => ({ type: "open", id, input })),
        audio.open(input).catch((error: unknown): AudioOpenResult => {
          reportError(`Audio: ${String(error)}`);
          return { ok: false, reason: "unsupported-format" };
        }),
      ]);
      hasAudio = video.result.ok && sound.ok;
      return { video: video.result, audio: sound };
    },
    play(from) {
      session++;
      if (hasAudio) audio.play(session, from);
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
      decode.terminate();
      audio.dispose();
    },
  };
}
