/** `MM:SS.cc` for a time in integer µs (centiseconds are truncated, never rounded up). */
export function formatTimecode(us: number): string {
  const cs = Math.floor(Math.max(0, us) / 10_000);
  const m = Math.floor(cs / 6000);
  const s = Math.floor(cs / 100) % 60;
  const f = cs % 100;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${String(f).padStart(2, "0")}`;
}

/** Parse `MM:SS.ff`, `M:SS`, or raw seconds into integer µs. */
export function parseTimecode(text: string): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  if (/^\d+(\.\d+)?$/.test(trimmed)) {
    const n = Number(trimmed);
    return Number.isFinite(n) && n >= 0 ? Math.round(n * 1_000_000) : null;
  }
  const m = trimmed.match(/^(\d+):(\d{1,2})(?:\.(\d{1,2}))?$/);
  if (!m) return null;
  const mins = Number(m[1]);
  const secs = Number(m[2]);
  const cs = m[3] ? Number(m[3].padEnd(2, "0")) : 0;
  if (!Number.isFinite(mins) || !Number.isFinite(secs) || secs >= 60) return null;
  return (mins * 60 + secs) * 1_000_000 + cs * 10_000;
}

export function fileName(path: string): string {
  return path.split(/[/\\]/).pop() ?? path;
}
