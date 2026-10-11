// Host-supplied assets for the wasm preview compositor (docs/preview-webview.md P4b).
//
// The wasm build of renderly-core has no filesystem, so data the native export reads from
// disk - raster / generated mattes and pack `.cube` LUTs - is fetched here and handed over.
// This file is plumbing only: WHICH files are needed (and their cache keys) is answered by
// the wasm module (`required_assets()`, core Rust), the bytes come from the Tauri backend,
// and every pixel/LUT operation (RGBA -> luma, .cube parsing, compositing) stays in
// renderly-core. Nothing here reimplements an effect or a mask.

/// The slice of `WasmCompositor` this module drives.
export interface PreviewAssetHost {
  required_assets(): string;
  set_matte(key: string, width: number, height: number, rgba: Uint8Array): void;
  remove_matte(key: string): void;
  set_pack_lut(packId: string, lutId: string, cubeText: string): void;
  clear_pack_luts(): void;
}

export interface PackLutPayload {
  pack_id: string;
  lut_id: string;
  cube: string;
}

/// Where the bytes come from (`lib/ipc.ts` in the app, a stub in tests).
export interface PreviewAssetBackend {
  readMatte(path: string): Promise<ArrayBuffer>;
  matteStamps(paths: string[]): Promise<number[]>;
  readPackLuts(wanted: { pack_id: string; lut_id: string }[]): Promise<PackLutPayload[]>;
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
  pack_luts: { pack_id: string; lut_id: string }[];
}

const lutKey = (packId: string, lutId: string) => `${packId}\u0000${lutId}`;

/// Keeps the compositor's host-supplied assets in step with the project. Call `sync` after
/// every `set_project`; calls coalesce (one in flight, one queued) so rapid project patches
/// (e.g. a mask drag) cost a single `required_assets()` walk and no IPC unless something
/// file-backed actually changed.
export class PreviewAssetSync {
  private readonly loadedMattes = new Map<string, { path: string; stamp: number }>();
  private readonly loadedLuts = new Set<string>();
  private packSignature = "";
  private running = false;
  private queued = false;
  private disposed = false;

  constructor(
    private readonly host: PreviewAssetHost,
    private readonly backend: PreviewAssetBackend,
    /// Fired after assets were added/replaced/removed so the engine can repaint.
    private readonly onChange: () => void,
    private readonly decode: (bytes: ArrayBuffer) => Promise<DecodedImage> = decodeImageToRgba,
    private readonly packPaths: () => readonly string[] = () => [],
  ) {}

  dispose(): void {
    this.disposed = true;
  }

  /// Forget everything (compositor recreated / project closed); the next `sync` reloads.
  reset(): void {
    this.loadedMattes.clear();
    this.loadedLuts.clear();
    this.packSignature = "";
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
    changed = (await this.syncMattes(required.mattes)) || changed;
    changed = (await this.syncPackLuts(required.pack_luts)) || changed;
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

  private async syncPackLuts(wanted: RequiredAssets["pack_luts"]): Promise<boolean> {
    let changed = false;
    const wantedKeys = new Set(wanted.map((l) => lutKey(l.pack_id, l.lut_id)));
    const signature = this.packPaths().join("\u0000");
    const lostSome = Array.from(this.loadedLuts).some((k) => !wantedKeys.has(k));
    if (signature !== this.packSignature || lostSome) {
      // Pack loaded/unloaded/swapped, or a LUT no longer used: rebuild from scratch (the
      // compositor also drops its uploaded 3D textures on clear).
      if (this.loadedLuts.size > 0) {
        this.host.clear_pack_luts();
        changed = true;
      }
      this.loadedLuts.clear();
      this.packSignature = signature;
    }
    const missing = wanted.filter((l) => !this.loadedLuts.has(lutKey(l.pack_id, l.lut_id)));
    if (missing.length === 0) return changed;
    try {
      const payloads = await this.backend.readPackLuts(missing);
      if (this.disposed) return changed;
      for (const p of payloads) {
        try {
          this.host.set_pack_lut(p.pack_id, p.lut_id, p.cube);
          changed = true;
        } catch (err) {
          console.error(`[preview] pack LUT ${p.pack_id}/${p.lut_id} rejected:`, err);
        }
      }
    } catch (err) {
      console.error("[preview] pack LUT load failed:", err);
    }
    // Attempted (even if the backend could not resolve some): don't re-ask every patch.
    for (const l of missing) this.loadedLuts.add(lutKey(l.pack_id, l.lut_id));
    return changed;
  }
}
