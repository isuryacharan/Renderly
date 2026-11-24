// Main-thread side of export: an export worker with its own pool of decode workers, so an export
// never competes with the preview's decoders.

import { startDecodeWorker, type MediaInput } from "@renderly/media";
import type { ExportResult, ExportSettings, FromExporter, ToExporter } from "./exporter";
import type { Cut } from "./timeline";

/** Decoders for export: up to three layers' current runs plus one run ahead. */
const EXPORT_DECODERS = 4;

export interface ExportProgress {
  frames: number;
  total: number;
  /** The export worker's JS heap, where the browser reports it. */
  heapMB: number | null;
  /** GPU memory the export accounts for. */
  gpuMB: number;
}

export interface ExportJob {
  readonly done: Promise<ExportResult>;
  /** Stops the export and removes the partial file. `done` resolves with "cancelled". */
  cancel(): void;
}

export interface Exporter {
  start(options: {
    file: FileSystemFileHandle;
    sources: MediaInput[];
    cuts: Cut[];
    settings: ExportSettings;
    onProgress?: (progress: ExportProgress) => void;
  }): ExportJob;
  /** Parity test: one frame composited by the export path, as 8-bit RGBA (rows bottom first). */
  renderFrame(
    sources: MediaInput[],
    cuts: Cut[],
    settings: ExportSettings,
    time: number,
  ): Promise<Uint8Array>;
  dispose(): void;
}

export function createExporter(): Exporter {
  const decoders = Array.from({ length: EXPORT_DECODERS }, () => startDecodeWorker());
  const worker = new Worker(new URL("./export-worker.ts", import.meta.url), {
    type: "module",
    name: "export",
  });
  const send = (message: ToExporter, transfer: Transferable[] = []) =>
    worker.postMessage(message, transfer);
  send(
    { type: "init", decoders: decoders.map((d) => d.port) },
    decoders.map((d) => d.port),
  );

  let nextId = 1;
  const handlers = new Map<number, (message: FromExporter) => void>();
  worker.onmessage = (event: MessageEvent<FromExporter>) => {
    handlers.get(event.data.id)?.(event.data);
  };

  return {
    start({ file, sources, cuts, settings, onProgress }) {
      const id = nextId++;
      const done = new Promise<ExportResult>((resolve) => {
        handlers.set(id, (message) => {
          if (message.type === "progress") {
            onProgress?.({
              frames: message.frames,
              total: message.total,
              heapMB: message.heapMB,
              gpuMB: message.gpuMB,
            });
          } else if (message.type === "done") {
            handlers.delete(id);
            resolve(message.result);
          }
        });
      });
      send({ type: "start", id, file, sources, cuts, settings });
      return { done, cancel: () => send({ type: "cancel", id }) };
    },
    renderFrame(sources, cuts, settings, time) {
      const id = nextId++;
      return new Promise((resolve, reject) => {
        handlers.set(id, (message) => {
          handlers.delete(id);
          if (message.type === "frame") resolve(message.pixels);
          else if (message.type === "error") reject(new Error(message.message));
        });
        send({ type: "render-frame", id, sources, cuts, settings, time });
      });
    },
    dispose() {
      worker.terminate();
      for (const decoder of decoders) decoder.terminate();
    },
  };
}
