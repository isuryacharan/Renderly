// Main-thread side of the preview: starts a decode worker and a render worker, connects them
// with a MessageChannel, and hands the canvas to the render worker. After that the main thread
// only sends commands and receives a stats message once a second.

import { startDecodeWorker, type Flicks, type MediaInput, type OpenResult } from "@renderly/media";
import type { PreviewScale, Size } from "./layout";
import type { FromRenderer, RenderStats, ToRenderer } from "./protocol";

export interface Preview {
  open(input: MediaInput): Promise<OpenResult>;
  play(from: Flicks): void;
  pause(): void;
  /** Shows the exact frame at `time`; resolves with the frame's start time, or null. */
  seek(time: Flicks): Promise<Flicks | null>;
  setPreviewScale(scale: PreviewScale): void;
  /** Test hook: loses the WebGL context and restores it after `restoreAfterMs`. */
  loseContext(restoreAfterMs: number): void;
  onStats(listener: (stats: RenderStats) => void): void;
  onError(listener: (message: string) => void): void;
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

export function createPreview(canvas: HTMLCanvasElement): Preview {
  const decode = startDecodeWorker();
  const render = new Worker(new URL("./render-worker.ts", import.meta.url), {
    type: "module",
    name: "render",
  });
  const send = (message: ToRenderer, transfer: Transferable[] = []) =>
    render.postMessage(message, transfer);

  let nextId = 1;
  const pending = new Map<
    number,
    { resolve: (message: FromRenderer) => void; reject: (error: Error) => void }
  >();
  const statsListeners: ((stats: RenderStats) => void)[] = [];
  const errorListeners: ((message: string) => void)[] = [];

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
        for (const listener of errorListeners) listener(message.message);
        return;
      case "opened":
      case "seeked":
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

  function request<T extends FromRenderer>(build: (id: number) => ToRenderer): Promise<T> {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve: (message) => resolve(message as T), reject });
      send(build(id));
    });
  }

  return {
    async open(input) {
      const reply = await request<Extract<FromRenderer, { type: "opened" }>>((id) => ({
        type: "open",
        id,
        input,
      }));
      return reply.result;
    },
    play: (from) => send({ type: "play", from }),
    pause: () => send({ type: "pause" }),
    async seek(time) {
      const reply = await request<Extract<FromRenderer, { type: "seeked" }>>((id) => ({
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
    },
  };
}
