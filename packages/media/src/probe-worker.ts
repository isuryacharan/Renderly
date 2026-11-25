// Worker for probing files on import. One message in, one result out; files are posted as Blobs
// (a File from a picker or a handle), which the worker reads by range.

import { probe, type ProbeOptions, type ProbeResult } from "./probe";

export interface ProbeRequest {
  id: number;
  file: Blob;
  options?: ProbeOptions;
}

export interface ProbeResponse {
  id: number;
  result: ProbeResult;
}

addEventListener("message", (event: MessageEvent<ProbeRequest>) => {
  const { id, file, options } = event.data;
  void probe(file, options).then(
    (result) => postMessage({ id, result } satisfies ProbeResponse),
    (error: unknown) =>
      postMessage({
        id,
        result: {
          ok: false,
          reason: "unreadable",
          detail: error instanceof Error ? error.message : String(error),
          head: "",
        },
      } satisfies ProbeResponse),
  );
});
