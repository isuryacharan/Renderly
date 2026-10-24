// A tiny registry so non-preview code (hover preview in the store) can ask the live preview
// engine to fetch pack files without importing the engine or reaching for a global.

interface PackItemEnsurer {
  ensurePackItems(refs: readonly string[]): Promise<void>;
  invalidatePackItems(): void;
}

let engine: PackItemEnsurer | null = null;

export function registerPreviewEngine(e: PackItemEnsurer | null): void {
  engine = e;
}

/** Make sure installed pack items named by `refs` are registered with the preview compositor
 *  (builtin refs need nothing). Fire-and-forget: the preview repaints when they arrive. */
export function ensurePreviewPackItems(refs: readonly string[]): void {
  if (refs.some((r) => r.startsWith("pack:"))) void engine?.ensurePackItems(refs);
}

export function invalidatePreviewPackItems(): void {
  engine?.invalidatePackItems();
}
