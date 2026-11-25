// A small pool of probe workers. Import hands it many files at once; each worker probes one at a
// time, so a folder of hundreds of files never opens hundreds of readers.

import type { ProbeOptions, ProbeResult } from "./probe";
import type { ProbeRequest, ProbeResponse } from "./probe-worker";

export interface Prober {
  probe(file: Blob, options?: ProbeOptions): Promise<ProbeResult>;
  close(): void;
}

export function createProber(
  workers = Math.min(4, Math.max(2, navigator.hardwareConcurrency >> 2)),
): Prober {
  const pool = Array.from(
    { length: workers },
    () =>
      new Worker(new URL("./probe-worker.ts", import.meta.url), { type: "module", name: "probe" }),
  );
  const waiting = new Map<number, (result: ProbeResult) => void>();
  const busy = new Map<Worker, number>(pool.map((w) => [w, 0]));
  let next = 0;
  for (const worker of pool) {
    worker.addEventListener("message", (event: MessageEvent<ProbeResponse>) => {
      busy.set(worker, (busy.get(worker) ?? 1) - 1);
      waiting.get(event.data.id)?.(event.data.result);
      waiting.delete(event.data.id);
    });
  }
  return {
    probe(file, options) {
      const id = next++;
      // The least busy worker takes it.
      const worker = pool.reduce((a, b) => ((busy.get(a) ?? 0) <= (busy.get(b) ?? 0) ? a : b));
      busy.set(worker, (busy.get(worker) ?? 0) + 1);
      return new Promise((resolve) => {
        waiting.set(id, resolve);
        worker.postMessage({ id, file, ...(options ? { options } : {}) } satisfies ProbeRequest);
      });
    },
    close() {
      for (const worker of pool) worker.terminate();
      for (const resolve of waiting.values())
        resolve({ ok: false, reason: "unreadable", detail: "The import was closed.", head: "" });
      waiting.clear();
    },
  };
}
