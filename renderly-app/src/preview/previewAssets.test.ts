import { describe, expect, it, vi } from "vitest";
import { PreviewAssetSync } from "./previewAssets";
import type { PreviewAssetBackend, PreviewAssetHost } from "./previewAssets";

function makeHost(required: () => object, frames?: (t: number) => object[]) {
  const calls: string[] = [];
  const host: PreviewAssetHost = {
    required_assets: () => JSON.stringify(required()),
    ...(frames ? { required_frame_mattes: (t: number) => JSON.stringify(frames(t)) } : {}),
    set_matte: (key, w, h) => void calls.push(`matte+ ${key} ${w}x${h}`),
    remove_matte: (key) => void calls.push(`matte- ${key}`),
    set_pack_lut: (p, l) => void calls.push(`lut+ ${p}/${l}`),
    clear_pack_luts: () => void calls.push("lut-clear"),
  };
  return { host, calls };
}

function makeBackend(stamp: { v: number }) {
  const backend: PreviewAssetBackend = {
    readMatte: vi.fn(async () => new ArrayBuffer(4)),
    matteStamps: vi.fn(async (paths: string[]) => paths.map(() => stamp.v)),
    readPackLuts: vi.fn(async (wanted: { pack_id: string; lut_id: string }[]) =>
      wanted.map((w) => ({ ...w, cube: "cube" })),
    ),
  };
  return backend;
}

const decode = async () => ({ width: 2, height: 1, rgba: new Uint8Array(8) });

describe("PreviewAssetSync", () => {
  it("loads mattes once, reloads on a new stamp, removes when unreferenced", async () => {
    const state = { mattes: [{ key: "raster:a.png", path: "a.png" }] };
    const { host, calls } = makeHost(() => ({ mattes: state.mattes, pack_luts: [] }));
    const stamp = { v: 1 };
    const backend = makeBackend(stamp);
    const onChange = vi.fn();
    const sync = new PreviewAssetSync(host, backend, onChange, decode);

    await sync.sync();
    await sync.sync(); // unchanged -> no reload
    expect(calls).toEqual(["matte+ raster:a.png 2x1"]);
    expect(backend.readMatte).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledTimes(1);

    stamp.v = 2; // matte regenerated at the same path
    await sync.sync();
    expect(calls).toEqual(["matte+ raster:a.png 2x1", "matte+ raster:a.png 2x1"]);

    state.mattes = [];
    await sync.sync();
    expect(calls[calls.length - 1]).toBe("matte- raster:a.png");
    expect(backend.matteStamps).toHaveBeenCalledTimes(3); // none asked when nothing wanted
  });

  it("does not touch the backend when the project references nothing file-backed", async () => {
    const { host, calls } = makeHost(() => ({ mattes: [], pack_luts: [] }));
    const backend = makeBackend({ v: 1 });
    const onChange = vi.fn();
    await new PreviewAssetSync(host, backend, onChange, decode).sync();
    expect(calls).toEqual([]);
    expect(backend.matteStamps).not.toHaveBeenCalled();
    expect(backend.readPackLuts).not.toHaveBeenCalled();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("fetches pack LUTs once, and rebuilds when the pack set changes", async () => {
    const state = {
      luts: [{ pack_id: "p", lut_id: "a" }],
      paths: ["C:/packs/p"],
    };
    const { host, calls } = makeHost(() => ({ mattes: [], pack_luts: state.luts }));
    const backend = makeBackend({ v: 1 });
    const sync = new PreviewAssetSync(host, backend, vi.fn(), decode, () => state.paths);

    await sync.sync();
    await sync.sync();
    expect(calls).toEqual(["lut+ p/a"]);
    expect(backend.readPackLuts).toHaveBeenCalledTimes(1);

    state.paths = ["C:/packs/p2"]; // pack swapped -> clear and reload
    await sync.sync();
    expect(calls.slice(1)).toEqual(["lut-clear", "lut+ p/a"]);

    state.luts = []; // effect removed -> clear
    await sync.sync();
    expect(calls[calls.length - 1]).toBe("lut-clear");
  });

  it("a failing matte load does not throw or retry on every sync", async () => {
    const { host, calls } = makeHost(() => ({
      mattes: [{ key: "raster:bad.png", path: "bad.png" }],
      pack_luts: [],
    }));
    const backend = makeBackend({ v: 1 });
    backend.readMatte = vi.fn(async () => {
      throw new Error("nope");
    });
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const sync = new PreviewAssetSync(host, backend, vi.fn(), decode);
    await sync.sync();
    await sync.sync();
    expect(calls).toEqual([]);
    expect(backend.readMatte).toHaveBeenCalledTimes(1);
    err.mockRestore();
  });

  it("registers the time-indexed matte frame per key, replaces it as time moves, and prefetches", async () => {
    // A clip whose source runs 1:1 with the timeline at 10 matte frames per second.
    const frame = (t: number) => {
      const index = Math.round(t * 10);
      return [{ key: "gen:c", path: `c/f${index}.png`, index, stamp: 7 }];
    };
    const { host, calls } = makeHost(
      () => ({ mattes: [], pack_luts: [], frame_mattes: true }),
      frame,
    );
    const backend = makeBackend({ v: 1 });
    const onChange = vi.fn();
    const sync = new PreviewAssetSync(host, backend, onChange, decode);

    // Before the project says it has per-frame mattes, nothing is looked up.
    sync.syncFrameMattes(0);
    await new Promise((r) => setTimeout(r, 0));
    expect(backend.readMatte).not.toHaveBeenCalled();

    await sync.sync();
    sync.syncFrameMattes(0);
    await vi.waitFor(() => expect(calls).toEqual(["matte+ gen:c 2x1"]));
    expect(onChange).toHaveBeenCalledTimes(1);
    // f0 registered; f1 (t+0.1) was prefetched into the cache but not registered.
    await vi.waitFor(() => expect(backend.readMatte).toHaveBeenCalledTimes(2));
    expect(calls).toHaveLength(1);

    // Same instant again: no work. Next frame: served from the prefetch cache, no new read.
    sync.syncFrameMattes(0);
    sync.syncFrameMattes(0.1);
    await vi.waitFor(() => expect(calls).toHaveLength(2));
    expect(backend.readMatte).toHaveBeenCalledWith("c/f0.png");
    expect(backend.readMatte).toHaveBeenCalledWith("c/f1.png");

    // Regenerated sequence (new stamp) invalidates what was registered.
    const regenerated = makeHost(
      () => ({ mattes: [], pack_luts: [], frame_mattes: true }),
      (t) => frame(t).map((r) => ({ ...r, stamp: 8 })),
    );
    const sync2 = new PreviewAssetSync(regenerated.host, makeBackend({ v: 1 }), vi.fn(), decode);
    await sync2.sync();
    sync2.syncFrameMattes(0);
    await vi.waitFor(() => expect(regenerated.calls).toHaveLength(1));
  });

  it("a missing matte frame is logged once and not retried on every draw", async () => {
    const { host, calls } = makeHost(
      () => ({ mattes: [], pack_luts: [], frame_mattes: true }),
      () => [{ key: "gen:c", path: "c/f0.png", index: 0, stamp: 1 }],
    );
    const backend = makeBackend({ v: 1 });
    backend.readMatte = vi.fn(async () => {
      throw new Error("gone");
    });
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const sync = new PreviewAssetSync(host, backend, vi.fn(), decode);
    await sync.sync();
    for (let i = 0; i < 3; i++) {
      sync.syncFrameMattes(0);
      await new Promise((r) => setTimeout(r, 0));
    }
    expect(calls).toEqual([]);
    expect(backend.readMatte).toHaveBeenCalledTimes(1);
    err.mockRestore();
  });
});
