import { describe, expect, it } from "vitest";
import { backoff } from "./autosave";
import { backupName, backupsToKeep, parseBackupName, type Backup } from "./backups";
import { folderName } from "./projects";

const DAY = 86_400_000;
const MIN = 60_000;

describe("backups", () => {
  it("names round-trip", () => {
    const time = Date.UTC(2026, 10, 25, 10, 30, 0, 7);
    const name = backupName(time, 42);
    expect(name).toBe("2026-11-25T10-30-00-007Z-r42.json");
    expect(parseBackupName(name)).toEqual({ name, time, revision: 42 });
    expect(parseBackupName("notes.txt")).toBeNull();
  });

  it("keeps the newest few and one a day for the last days", () => {
    const now = Date.UTC(2026, 10, 25, 12);
    const backups: Backup[] = [];
    // Every 5 minutes for 3 hours today, plus four a day for the 20 days before.
    for (let i = 0; i < 36; i++) backups.push(make(now - i * 5 * MIN, 1000 - i));
    for (let d = 1; d <= 20; d++)
      for (let h = 0; h < 4; h++) backups.push(make(now - d * DAY - h * 3600_000, 500 - d * 4 - h));
    const keep = backupsToKeep(backups, now, { interval: 5 * MIN, recent: 10, days: 14 });
    const kept = backups.filter((b) => keep.has(b.name));
    // 10 most recent (all today) + the newest of each of the 14 earlier days within range.
    expect(kept.filter((b) => now - b.time < DAY)).toHaveLength(10);
    const older = kept.filter((b) => now - b.time >= DAY);
    expect(older).toHaveLength(14);
    expect(new Set(older.map((b) => Math.floor(b.time / DAY))).size).toBe(14);
    expect(Math.max(...older.map((b) => now - b.time))).toBeLessThanOrEqual(14 * DAY);
  });

  it("keeps everything when there are few", () => {
    const now = Date.UTC(2026, 0, 1);
    const backups = [make(now, 3), make(now - MIN, 2), make(now - 40 * DAY, 1)];
    expect(backupsToKeep(backups, now, { interval: 0, recent: 20, days: 14 }).size).toBe(3);
  });
});

function make(time: number, revision: number): Backup {
  return { name: backupName(time, revision), time, revision };
}

it("retries with a doubling delay up to 30 s", () => {
  expect([1, 2, 3, 4, 5, 6, 7, 20].map(backoff)).toEqual([
    1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000,
  ]);
});

it("names project folders after the project", () => {
  expect(folderName("Holiday edit!", "d27a5e00aaaaaaaa")).toBe("holiday-edit-d27a5e");
  expect(folderName("Café — Été", "abcdef0123456789")).toBe("cafe-ete-abcdef");
  expect(folderName("???", "abcdef0123456789")).toBe("project-abcdef");
});
