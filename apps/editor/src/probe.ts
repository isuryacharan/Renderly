// What the editor needs from the browser, checked at start-up. A missing feature stops the app
// with a plain message instead of failing later in the middle of an edit.

export interface Missing {
  feature: string;
  why: string;
}

export function probe(): Missing[] {
  const missing: Missing[] = [];
  const need = (ok: boolean, feature: string, why: string) => ok || missing.push({ feature, why });
  const g = globalThis as Record<string, unknown>;
  need("VideoDecoder" in g && "VideoEncoder" in g, "WebCodecs", "to decode and encode video");
  need(webgl2(), "WebGL 2", "to draw the picture");
  need("AudioWorkletNode" in g, "Audio worklets", "to play sound in time with the picture");
  need(
    g["crossOriginIsolated"] === true && "SharedArrayBuffer" in g,
    "Shared memory",
    "to keep sound and picture in sync",
  );
  need(
    typeof navigator.storage?.getDirectory === "function",
    "Browser file storage",
    "to keep projects",
  );
  need(
    typeof FileSystemFileHandle !== "undefined" &&
      "createWritable" in FileSystemFileHandle.prototype,
    "Writable files",
    "to save projects safely",
  );
  need("locks" in navigator, "Web Locks", "so two tabs never edit one project at once");
  need("BroadcastChannel" in g, "Tab messaging", "so other tabs see your changes");
  return missing;
}

function webgl2(): boolean {
  try {
    return document.createElement("canvas").getContext("webgl2") !== null;
  } catch {
    return false;
  }
}
