// Pure math for the generic effect-parameter rows (no React), so it is unit-testable.

import type { CatalogueParam } from "./ipc";

/** Slider step for a numeric range: whole numbers for `int`/`choice`, ~200 nice stops otherwise. */
export function paramStep(spec: Pick<CatalogueParam, "type" | "min" | "max">): number {
  if (spec.type === "int" || spec.type === "choice") return 1;
  const span = (spec.max ?? 1) - (spec.min ?? 0);
  if (!Number.isFinite(span) || span <= 0) return 0.01;
  const raw = span / 200;
  const pow = Math.pow(10, Math.floor(Math.log10(raw)));
  const norm = raw / pow;
  const nice = norm < 1.5 ? 1 : norm < 3.5 ? 2 : norm < 7.5 ? 5 : 10;
  return nice * pow;
}

/** A value formatted to the precision of its step. */
export function formatParam(n: number, step: number): string {
  const decimals = step >= 1 ? 0 : Math.min(4, Math.max(0, Math.ceil(-Math.log10(step))));
  return n.toFixed(decimals);
}

const clamp01 = (n: number) => Math.min(1, Math.max(0, n));

/** `[r, g, b, a]` (0..1) to `#rrggbb` (alpha is edited separately). */
export function colorToHex(c: number[]): string {
  const h = (v: number | undefined) =>
    Math.round(clamp01(v ?? 0) * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${h(c[0])}${h(c[1])}${h(c[2])}`;
}

export function hexToColor(hex: string, alpha: number): number[] {
  const n = parseInt(hex.slice(1), 16);
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255, alpha];
}

/** Display name for a param key or choice id: `soft_light` -> `Soft light`. */
export function prettyName(id: string): string {
  return id.replace(/[_-]+/g, " ").replace(/^./, (c) => c.toUpperCase());
}
