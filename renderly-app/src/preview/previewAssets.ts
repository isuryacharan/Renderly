// Host-supplied assets for the wasm preview compositor (docs/preview-webview.md P4b).
//
// The wasm build of renderly-core has no filesystem, so data the native export reads from
// disk - raster / generated mattes and the files of pack items (LUTs, effects, filters,
// transitions) - is fetched here and handed over.
// This file is plumbing only: WHICH files are needed (and their cache keys) is answered by
// the wasm module (`required_assets()`, core Rust), the bytes come from the Tauri backend,
// and every pixel/LUT operation (RGBA -> luma, .cube parsing, compositing) stays in
// renderly-core. Nothing here reimplements an effect or a mask.

/// The slice of `WasmCompositor` this module drives.
export interface PreviewAssetHost {
  required_assets(): string;
  /// Per-frame background-removal mattes the layers visible at timeline time `t` need (JSON
  /// `FrameMatteRequest[]`). Absent on older wasm builds.
  required_frame_mattes?(timeUs: number): string;
  set_matte(key: string, width: number, height: number, rgba: Uint8Array): void;
  remove_matte(key: string): void;
  /// Register (replace) one pack item. `itemJson` is the backend's item minus its textures;
  /// `textures` are the effect's file textures decoded to RGBA (or undefined).
  set_pack_item(itemJson: string, textures: PackTexture[] | undefined): void;
  clear_pack_items(): void;
}

/// A decoded file texture of an effect, as `WasmCompositor.set_pack_item` takes it.
export interface PackTexture {
  path: string;
  width: number;
  height: number;
  rgba: Uint8Array;
}

/// A pack item as the backend returns it (`read_preview_pack_items`).
export interface PackItemPayload {
  ref: string;
  pack_id: string;
  item_id: string;
  version: number;
  kind: string;
  hash?: string | null;
  cube?: string;
  manifest?: string;
  shaders: Record<string, string>;
  textures: { path: string; bytes_base64: string }[];
}

/// Where the bytes come from (`lib/ipc.ts` in the app, a stub in tests).
export interface PreviewAssetBackend {
  readMatte(path: string): Promise<ArrayBuffer>;
  matteStamps(paths: string[]): Promise<number[]>;
  /// `includeInstalled` also serves installed items the project does not use yet (hover preview).
  readPackItems(
    wanted: { ref: string; hash?: string | null }[],
    includeInstalled?: boolean,
  ): Promise<PackItemPayload[]>;
}

export interface DecodedImage {
  width: number;
  height: number;
  rgba: Uint8Array;
}

/// Browser image decode to straight RGBA, with colour management and alpha premultiplication
/// off so a gray matte's bytes survive unchanged.
export async function decodeImageToRgba(bytes: ArrayBuffer): Promise<DecodedImage> {
  const bitmap = await createImageBitmap(new Blob([bytes]), {
    colorSpaceConversion: "none",
    premultiplyAlpha: "none",
  });
  try {
    const { width, height } = bitmap;
    const canvas =
      typeof OffscreenCanvas !== "undefined"
        ? new OffscreenCanvas(width, height)
        : Object.assign(document.createElement("canvas"), { width, height });
    const ctx = canvas.getContext("2d", { willReadFrequently: true }) as
      | CanvasRenderingContext2D
      | OffscreenCanvasRenderingContext2D
      | null;
    if (!ctx) throw new Error("2d context unavailable for matte decode");
    ctx.drawImage(bitmap, 0, 0);
    const data = ctx.getImageData(0, 0, width, height).data;
    return { width, height, rgba: new Uint8Array(data.buffer, data.byteOffset, data.byteLength) };
  } finally {
    bitmap.close();
  }
}

interface RequiredAssets {
  mattes: { key: string; path: string }[];
  /// The `pack:` refs of the effects, filters, transitions and LUTs the project uses, each with
  /// the hash stored where it was applied.
  pack_items: { ref: string; hash?: string | null }[];
  /// Some clip has a per-frame (time-indexed) background-removal matte.
  frame_mattes?: boolean;
}

/// One matte frame a layer needs right now (core's `mask::frame_matte_requests_at`).
interface FrameMatteRequest {
  key: string;
  path: string;
  index: number;
  stamp: number;
}

/// How far ahead (timeline µs) matte frames are decoded into the cache during playback.
const FRAME_MATTE_PREFETCH_US = 100_000;
/// Decoded matte frames kept (720p RGBA is ~3.7 MB each).
const FRAME_MATTE_CACHE = 6;

const itemKey = (ref: string, hash?: string | null) => `${ref}\u0000${hash ?? ""}`;

function base64ToArrayBuffer(b64: string): ArrayBuffer {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes.buffer;
}

/// Keeps the compositor's host-supplied assets in step with the project. Call `sync` after
/// every `set_project`; calls coalesce (one in flight, one queued) so rapid project patches
/// (e.g. a mask drag) cost a single `required_assets()` walk and no IPC unless something
/// file-backed actually changed.
export class PreviewAssetSync {
  private readonly loadedMattes = new Map<string, { path: string; stamp: number }>();
  /// Pack items the project uses that were fetched (or attempted), by `ref` + hash.
  private readonly loadedItems = new Set<string>();
  /// Installed items fetched for hover preview only (not in the project yet), by ref.
  private readonly hoverItems = new Set<string>();
  /// Per-frame mattes (time-indexed): which frame is registered per key, decoded frames kept
  /// for scrubbing / prefetch, and the single-flight state of the loader.
  private frameMattesWanted = false;
  private readonly frameLoaded = new Map<string, string>();
  private readonly frameDecoded = new Map<string, DecodedImage>();
  private frameBusy = false;
  private frameQueued: number | null = null;
  private running = false;
  private queued = false;
  private disposed = false;

  constructor(
    private readonly host: PreviewAssetHost,
    private readonly backend: PreviewAssetBackend,
    /// Fired after assets were added/replaced/removed so the engine can repaint.
    private readonly onChange: () => void,
    private readonly decode: (bytes: ArrayBuffer) => Promise<DecodedImage> = decodeImageToRgba,
  ) {}

  dispose(): void {
    this.disposed = true;
  }

  /// Forget everything (compositor recreated / project closed); the next `sync` reloads.
  reset(): void {
    this.loadedMattes.clear();
    this.loadedItems.clear();
    this.hoverItems.clear();
    this.frameLoaded.clear();
    this.frameDecoded.clear();
  }

  /// Make sure the per-frame background-removal matte(s) for timeline time `t` are registered
  /// with the compositor (a no-op unless some clip has one). Fire-and-forget and coalescing:
  /// scrubbing/playback call this every draw; only the latest time is loaded, and `onChange`
  /// repaints when a new matte frame was registered. Which frame a layer needs (source time ->
  /// nearest matte frame) is decided in core Rust (`required_frame_mattes`); this only moves
  /// bytes.
  syncFrameMattes(t: number): void {
    if (!this.frameMattesWanted || !this.host.required_frame_mattes || this.disposed) return;
    if (this.frameBusy) {
      this.frameQueued = t;
      return;
    }
    this.frameBusy = true;
    void this.runFrameMattes(t);
  }

  private async runFrameMattes(first: number): Promise<void> {
    try {
      let next: number | null = first;
      while (next !== null && !this.disposed) {
        this.frameQueued = null;
        if (await this.registerFrameMattes(next)) this.onChange();
        if (this.frameQueued === null) {
          await this.prefetchFrameMattes(next + FRAME_MATTE_PREFETCH_US);
        }
        next = this.frameQueued;
      }
    } finally {
      this.frameBusy = false;
    }
  }

  private requestsAt(t: number): FrameMatteRequest[] {
    try {
      return JSON.parse(this.host.required_frame_mattes?.(t) ?? "[]") as FrameMatteRequest[];
    } catch (err) {
      console.error("[preview] required_frame_mattes failed:", err);
      return [];
    }
  }

  private async decodeFrame(r: FrameMatteRequest): Promise<DecodedImage> {
    const sig = `${r.path}|${r.stamp}`;
    const hit = this.frameDecoded.get(sig);
    if (hit) {
      // Refresh recency.
      this.frameDecoded.delete(sig);
      this.frameDecoded.set(sig, hit);
      return hit;
    }
    const decoded = await this.decode(await this.backend.readMatte(r.path));
    this.frameDecoded.set(sig, decoded);
    while (this.frameDecoded.size > FRAME_MATTE_CACHE) {
      const oldest = this.frameDecoded.keys().next().value;
      if (oldest === undefined) break;
      this.frameDecoded.delete(oldest);
    }
    return decoded;
  }

  private async registerFrameMattes(t: number): Promise<boolean> {
    let changed = false;
    for (const r of this.requestsAt(t)) {
      const sig = `${r.path}|${r.stamp}`;
      if (this.frameLoaded.get(r.key) === sig) continue;
      try {
        const decoded = await this.decodeFrame(r);
        if (this.disposed) return changed;
        this.host.set_matte(r.key, decoded.width, decoded.height, decoded.rgba);
        changed = true;
      } catch (err) {
        // Missing frame: keep whatever matte is registered; record it so we do not retry on
        // every draw.
        console.error(`[preview] matte frame load failed (${r.path}):`, err);
      }
      this.frameLoaded.set(r.key, sig);
    }
    return changed;
  }

  private async prefetchFrameMattes(t: number): Promise<void> {
    for (const r of this.requestsAt(t)) {
      if (this.frameQueued !== null || this.disposed) return;
      if (this.frameLoaded.get(r.key) === `${r.path}|${r.stamp}`) continue;
      try {
        await this.decodeFrame(r);
      } catch {
        // Prefetch is best effort.
      }
    }
  }

  async sync(): Promise<void> {
    if (this.running) {
      this.queued = true;
      return;
    }
    this.running = true;
    try {
      do {
        this.queued = false;
        if (this.disposed) return;
        await this.syncOnce();
      } while (this.queued && !this.disposed);
    } finally {
      this.running = false;
    }
  }

  private async syncOnce(): Promise<void> {
    let required: RequiredAssets;
    try {
      required = JSON.parse(this.host.required_assets()) as RequiredAssets;
    } catch (err) {
      console.error("[preview] required_assets failed:", err);
      return;
    }
    let changed = false;
    this.frameMattesWanted = required.frame_mattes === true;
    if (!this.frameMattesWanted) {
      this.frameLoaded.clear();
      this.frameDecoded.clear();
    }
    changed = (await this.syncMattes(required.mattes)) || changed;
    changed = (await this.syncPackItems(required.pack_items ?? [])) || changed;
    if (changed && !this.disposed) this.onChange();
  }

  private async syncMattes(wanted: RequiredAssets["mattes"]): Promise<boolean> {
    let changed = false;
    const wantedKeys = new Set(wanted.map((m) => m.key));
    for (const key of Array.from(this.loadedMattes.keys())) {
      if (!wantedKeys.has(key)) {
        this.loadedMattes.delete(key);
        this.host.remove_matte(key);
        changed = true;
      }
    }
    if (wanted.length === 0) return changed;

    // Stamps catch a matte regenerated in place (same path, project JSON unchanged).
    let stamps: number[] = [];
    try {
      stamps = await this.backend.matteStamps(wanted.map((m) => m.path));
    } catch (err) {
      console.error("[preview] matte stamps failed:", err);
    }
    for (let i = 0; i < wanted.length; i++) {
      const { key, path } = wanted[i];
      const stamp = stamps[i] ?? 0;
      const have = this.loadedMattes.get(key);
      if (have && have.path === path && have.stamp === stamp) continue;
      try {
        const decoded = await this.decode(await this.backend.readMatte(path));
        if (this.disposed) return changed;
        this.host.set_matte(key, decoded.width, decoded.height, decoded.rgba);
        this.loadedMattes.set(key, { path, stamp });
        // The representative matte replaced whatever frame was registered under this key.
        this.frameLoaded.delete(key);
        changed = true;
      } catch (err) {
        // Missing/undecodable matte: the layer renders unmasked (export would fail loudly).
        // Record the attempt so a broken file is not re-read on every project patch.
        console.error(`[preview] matte load failed (${path}):`, err);
        this.loadedMattes.set(key, { path, stamp });
      }
    }
    return changed;
  }

  /// The library changed (a pack installed, updated or removed): drop every registered pack item
  /// so the next `sync` re-fetches what the project uses.
  invalidatePackItems(): void {
    if (this.loadedItems.size > 0 || this.hoverItems.size > 0) this.host.clear_pack_items();
    this.loadedItems.clear();
    this.hoverItems.clear();
  }

  private async syncPackItems(wanted: RequiredAssets["pack_items"]): Promise<boolean> {
    let changed = false;
    const wantedKeys = new Set(wanted.map((i) => itemKey(i.ref, i.hash)));
    const lostSome = Array.from(this.loadedItems).some((k) => !wantedKeys.has(k));
    if (lostSome) {
      // An item no longer used (or replaced by another version): rebuild from scratch (the
      // compositor also drops its uploaded textures on clear). Hover items go with it.
      this.host.clear_pack_items();
      this.loadedItems.clear();
      this.hoverItems.clear();
      changed = true;
    }
    const missing = wanted.filter((i) => !this.loadedItems.has(itemKey(i.ref, i.hash)));
    if (missing.length === 0) return changed;
    if (await this.fetchAndRegister(missing, false)) changed = true;
    // Attempted (even if the backend could not resolve some): don't re-ask every patch.
    for (const i of missing) this.loadedItems.add(itemKey(i.ref, i.hash));
    return changed;
  }

  private async fetchAndRegister(
    wanted: { ref: string; hash?: string | null }[],
    includeInstalled: boolean,
  ): Promise<boolean> {
    let changed = false;
    try {
      const payloads = await this.backend.readPackItems(wanted, includeInstalled);
      if (this.disposed) return changed;
      for (const p of payloads) {
        try {
          const { textures, ...item } = p;
          const decoded: PackTexture[] = [];
          for (const t of textures ?? []) {
            const img = await this.decode(base64ToArrayBuffer(t.bytes_base64));
            decoded.push({ path: t.path, width: img.width, height: img.height, rgba: img.rgba });
          }
          if (this.disposed) return changed;
          this.host.set_pack_item(JSON.stringify(item), decoded.length > 0 ? decoded : undefined);
          changed = true;
        } catch (err) {
          console.error(`[preview] pack item ${p.ref} rejected:`, err);
        }
      }
    } catch (err) {
      console.error("[preview] pack item load failed:", err);
    }
    return changed;
  }

  /// Make sure installed pack items the project does not use yet are registered, for hover
  /// preview. Builtin refs need nothing (compiled into the wasm build). Resolves true when a
  /// new item was registered (the caller repaints). Failures leave the item pass-through.
  async ensurePackItems(refs: readonly string[]): Promise<boolean> {
    const wanted = refs
      .filter((r) => r.startsWith("pack:"))
      .filter((r) => !this.hoverItems.has(r))
      .filter((r) => !Array.from(this.loadedItems).some((k) => k.startsWith(`${r}\u0000`)));
    if (wanted.length === 0 || this.disposed) return false;
    const unique = Array.from(new Set(wanted));
    const changed = await this.fetchAndRegister(
      unique.map((ref) => ({ ref })),
      true,
    );
    for (const r of unique) this.hoverItems.add(r);
    if (changed && !this.disposed) this.onChange();
    return changed;
  }
}
