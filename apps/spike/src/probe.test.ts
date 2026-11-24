import { describe, expect, it } from "vitest";
import { probeCapabilities } from "./probe";

const availability = (scope: object) =>
  Object.fromEntries(probeCapabilities(scope).map((c) => [c.name, c.available]));

// Only presence matters to the probe, so any value stands in for a constructor.
const stub = () => undefined;

const everything = {
  crossOriginIsolated: true,
  SharedArrayBuffer,
  VideoDecoder: stub,
  VideoEncoder: stub,
  VideoFrame: stub,
  AudioDecoder: stub,
  AudioEncoder: stub,
  WebGL2RenderingContext: stub,
  OffscreenCanvas: stub,
};

describe("probeCapabilities", () => {
  it("reports everything available in a capable, isolated browser", () => {
    expect(Object.values(availability(everything)).every(Boolean)).toBe(true);
  });

  it("reports nothing available on an empty scope", () => {
    expect(Object.values(availability({})).some(Boolean)).toBe(false);
  });

  it("treats SharedArrayBuffer as unavailable without cross-origin isolation", () => {
    const result = availability({ ...everything, crossOriginIsolated: false });
    expect(result["crossOriginIsolated"]).toBe(false);
    expect(result["SharedArrayBuffer"]).toBe(false);
  });

  it("needs decoder, encoder and frame for WebCodecs", () => {
    expect(availability({ ...everything, VideoEncoder: undefined })["WebCodecs"]).toBe(false);
  });
});
