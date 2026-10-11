import { describe, expect, it, vi } from "vitest";
import { PreviewAssetSync } from "./previewAssets";
import type { PreviewAssetBackend, PreviewAssetHost } from "./previewAssets";

function makeHost(required: () => object) {
  const calls: string[] = [];
  const host: PreviewAssetHost = {
    required_assets: () => JSON.stringify(required()),
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
});
