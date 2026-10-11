export function formatTimecode(secs: number): string {
  // Truncate on whole centiseconds; the epsilon absorbs binary float error so e.g. 0.29
  // (0.29 % 1 * 100 = 28.999...) reads .29 rather than .28.
  const cs = Math.floor(secs * 100 + 1e-6);
  const m = Math.floor(cs / 6000);
  const s = Math.floor(cs / 100) % 60;
  const f = cs % 100;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${String(f).padStart(2, "0")}`;
}

/** Parse `MM:SS.ff`, `M:SS`, or raw seconds into timeline seconds. */
export function parseTimecode(text: string): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  if (/^\d+(\.\d+)?$/.test(trimmed)) {
    const n = Number(trimmed);
    return Number.isFinite(n) && n >= 0 ? n : null;
  }
  const m = trimmed.match(/^(\d+):(\d{1,2})(?:\.(\d{1,2}))?$/);
  if (!m) return null;
  const mins = Number(m[1]);
  const secs = Number(m[2]);
  const frac = m[3] ? Number(m[3].padEnd(2, "0")) / 100 : 0;
  if (!Number.isFinite(mins) || !Number.isFinite(secs) || secs >= 60) return null;
  return mins * 60 + secs + frac;
}

export function fileName(path: string): string {
  return path.split(/[/\\]/).pop() ?? path;
}
