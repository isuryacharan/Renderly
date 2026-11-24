// Capability probe for the engine spike. Pure: it reads only the global object it is given,
// so it can be unit-tested without a browser.

export interface Capability {
  name: string;
  available: boolean;
  /** Why it matters to the engine. */
  purpose: string;
}

export function probeCapabilities(scope: object): Capability[] {
  const has = (key: string) => key in scope && Reflect.get(scope, key) !== undefined;
  const isolated = Reflect.get(scope, "crossOriginIsolated") === true;

  return [
    {
      name: "crossOriginIsolated",
      available: isolated,
      purpose: "SharedArrayBuffer for the audio ring buffer",
    },
    {
      name: "SharedArrayBuffer",
      available: isolated && has("SharedArrayBuffer"),
      purpose: "Audio clock shared between worker and worklet",
    },
    {
      name: "WebCodecs",
      available: has("VideoDecoder") && has("VideoEncoder") && has("VideoFrame"),
      purpose: "Decode and encode video",
    },
    {
      name: "WebCodecs audio",
      available: has("AudioDecoder") && has("AudioEncoder"),
      purpose: "Decode and encode audio",
    },
    {
      name: "WebGL2",
      available: has("WebGL2RenderingContext"),
      purpose: "Compositor",
    },
    {
      name: "OffscreenCanvas",
      available: has("OffscreenCanvas"),
      purpose: "Render from a worker",
    },
  ];
}

/** Confirms that a WebGL2 context can actually be created, not only that the API exists. */
export function canCreateWebGL2(): boolean {
  if (typeof OffscreenCanvas === "undefined") return false;
  return new OffscreenCanvas(1, 1).getContext("webgl2") !== null;
}
