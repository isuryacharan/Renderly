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
    set_pack_item: (json, textures) => {
      const item = JSON.parse(json) as { ref: string };
      calls.push(`item+ ${item.ref}${textures ? ` tex=${textures.map((t) => `${t.path}:${t.width}x${t.height}`).join(",")}` : ""}`);
    },
    clear_pack_items: () => void calls.push("items-clear"),
  };
  return { host, calls };
}

function makeBackend(stamp: { v: number }) {
  const backend: PreviewAssetBackend = {
    readMatte: vi.fn(async () => new ArrayBuffer(4)),
    matteStamps: vi.fn(async (paths: string[]) => paths.map(() => stamp.v)),
    readPackItems: vi.fn(async (wanted: { ref: string; hash?: string | null }[]) =>
      wanted.map((w) => ({
        ref: w.ref,
        pack_id: "p",
        item_id: w.ref.split("/")[1] ?? "x",
        version: 1,
        kind: w.ref.includes("lut") ? "lut" : "effect",
        hash: w.hash ?? null,
        shaders: {},
        textures: w.ref.includes("tex") ? [{ path: "t.png", bytes_base64: btoa("png") }] : [],
      })),
    ),
  };
  return backend;
}

const decode = async () => ({ width: 2, height: 1, rgba: new Uint8Array(8) });

describe("PreviewAssetSync", () => {
  it("loads mattes once, reloads on a new stamp, removes when unreferenced", async () => {
    const state = { mattes: [{ key: "raster:a.png", path: "a.png" }] };
    const { host, calls } = makeHost(() => ({ mattes: state.mattes, pack_items: [] }));
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
    const { host, calls } = makeHost(() => ({ mattes: [], pack_items: [] }));
    const backend = makeBackend({ v: 1 });
    const onChange = vi.fn();
    await new PreviewAssetSync(host, backend, onChange, decode).sync();
    expect(calls).toEqual([]);
    expect(backend.matteStamps).not.toHaveBeenCalled();
    expect(backend.readPackItems).not.toHaveBeenCalled();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("fetches pack items once, and rebuilds when an item is no longer used", async () => {
    const state = { items: [{ ref: "pack:p/a@1", hash: "sha256:1" }] };
    const { host, calls } = makeHost(() => ({ mattes: [], pack_items: state.items }));
    const backend = makeBackend({ v: 1 });
    const sync = new PreviewAssetSync(host, backend, vi.fn(), decode);

    await sync.sync();
    await sync.sync();
    expect(calls).toEqual(["item+ pack:p/a@1"]);
    expect(backend.readPackItems).toHaveBeenCalledTimes(1);
    expect(backend.readPackItems).toHaveBeenCalledWith([{ ref: "pack:p/a@1", hash: "sha256:1" }], false);

    // A second item is fetched alone; the first stays registered.
    state.items = [...state.items, { ref: "pack:p/b@1", hash: "sha256:2" }];
    await sync.sync();
    expect(calls.slice(1)).toEqual(["item+ pack:p/b@1"]);

    // Item removed from the project: clear and reload what is still used.
    state.items = [state.items[1]];
    await sync.sync();
    expect(calls.slice(2)).toEqual(["items-clear", "item+ pack:p/b@1"]);

    state.items = [];
    await sync.sync();
    expect(calls[calls.length - 1]).toBe("items-clear");
  });

  it("decodes an effect's textures to RGBA before set_pack_item", async () => {
    const { host, calls } = makeHost(() => ({
      mattes: [],
      pack_items: [{ ref: "pack:p/tex@1", hash: null }],
    }));
    const backend = makeBackend({ v: 1 });
    await new PreviewAssetSync(host, backend, vi.fn(), decode).sync();
    expect(calls).toEqual(["item+ pack:p/tex@1 tex=t.png:2x1"]);
  });

  it("a library change (invalidatePackItems) clears and refetches the project's items", async () => {
    const { host, calls } = makeHost(() => ({
      mattes: [],
      pack_items: [{ ref: "pack:p/a@1", hash: null }],
    }));
    const backend = makeBackend({ v: 1 });
    const sync = new PreviewAssetSync(host, backend, vi.fn(), decode);
    await sync.sync();
    sync.invalidatePackItems();
    await sync.sync();
    expect(calls).toEqual(["item+ pack:p/a@1", "items-clear", "item+ pack:p/a@1"]);
  });

  it("hover preview fetches installed pack items the project does not use, once, skipping builtins", async () => {
    const { host, calls } = makeHost(() => ({ mattes: [], pack_items: [] }));
    const backend = makeBackend({ v: 1 });
    const onChange = vi.fn();
    const sync = new PreviewAssetSync(host, backend, onChange, decode);

    expect(await sync.ensurePackItems(["builtin:blur@1"])).toBe(false);
    expect(backend.readPackItems).not.toHaveBeenCalled();

    expect(await sync.ensurePackItems(["pack:p/wipe@1", "builtin:blur@1"])).toBe(true);
    expect(backend.readPackItems).toHaveBeenCalledWith([{ ref: "pack:p/wipe@1" }], true);
    expect(calls).toEqual(["item+ pack:p/wipe@1"]);
    expect(onChange).toHaveBeenCalledTimes(1);

    // Hovered again: already registered.
    expect(await sync.ensurePackItems(["pack:p/wipe@1"])).toBe(false);
    expect(backend.readPackItems).toHaveBeenCalledTimes(1);

    // A project sync does not throw the hover item away.
    await sync.sync();
    expect(calls).toEqual(["item+ pack:p/wipe@1"]);
  });

  it("a failing matte load does not throw or retry on every sync", async () => {
    const { host, calls } = makeHost(() => ({
      mattes: [{ key: "raster:bad.png", path: "bad.png" }],
      pack_items: [],
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
      () => ({ mattes: [], pack_items: [], frame_mattes: true }),
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
      () => ({ mattes: [], pack_items: [], frame_mattes: true }),
      (t) => frame(t).map((r) => ({ ...r, stamp: 8 })),
    );
    const sync2 = new PreviewAssetSync(regenerated.host, makeBackend({ v: 1 }), vi.fn(), decode);
    await sync2.sync();
    sync2.syncFrameMattes(0);
    await vi.waitFor(() => expect(regenerated.calls).toHaveLength(1));
  });

  it("a missing matte frame is logged once and not retried on every draw", async () => {
    const { host, calls } = makeHost(
      () => ({ mattes: [], pack_items: [], frame_mattes: true }),
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
