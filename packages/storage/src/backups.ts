// Rolling backups of project.json in backups/. A save makes a backup when the last one is older
// than `interval`. Retention keeps the newest `recent` backups, plus the newest of each day for
// the last `days` days; the rest are deleted.

import { list, readText, remove, subdir, writeText, type Dir } from "./fs";

export interface BackupPolicy {
  /** Minimum time between backups, ms. */
  interval: number;
  recent: number;
  days: number;
}

export const DEFAULT_BACKUPS: BackupPolicy = { interval: 5 * 60_000, recent: 20, days: 14 };

export interface Backup {
  name: string;
  time: number;
  revision: number;
}

const DAY = 86_400_000;

/** `2026-11-25T10-30-00-000Z-r42.json` for a save at that time and revision. */
export function backupName(time: number, revision: number): string {
  return `${new Date(time).toISOString().replace(/[:.]/g, "-")}-r${revision}.json`;
}

export function parseBackupName(name: string): Backup | null {
  const match = /^(\d{4}-\d\d-\d\d)T(\d\d)-(\d\d)-(\d\d)-(\d{3})Z-r(\d+)\.json$/.exec(name);
  if (!match) return null;
  const [, date, h, m, s, ms, revision] = match;
  const time = Date.parse(`${date}T${h}:${m}:${s}.${ms}Z`);
  return Number.isNaN(time) ? null : { name, time, revision: Number(revision) };
}

/** Which backups the policy keeps (by name), given all of them. */
export function backupsToKeep(
  backups: readonly Backup[],
  now: number,
  policy: BackupPolicy,
): Set<string> {
  const newest = [...backups].sort((a, b) => b.time - a.time || b.revision - a.revision);
  const keep = new Set(newest.slice(0, policy.recent).map((b) => b.name));
  const days = new Set<number>();
  for (const backup of newest) {
    const day = Math.floor(backup.time / DAY);
    if (now - backup.time > policy.days * DAY || days.has(day)) continue;
    days.add(day);
    keep.add(backup.name);
  }
  return keep;
}

export async function listBackups(project: Dir): Promise<Backup[]> {
  const dir = await subdir(project, "backups");
  const backups: Backup[] = [];
  for (const entry of await list(dir)) {
    const backup = entry.kind === "file" ? parseBackupName(entry.name) : null;
    if (backup) backups.push(backup);
  }
  return backups.sort((a, b) => b.time - a.time || b.revision - a.revision);
}

export async function readBackup(project: Dir, name: string): Promise<string> {
  const text = await readText(await subdir(project, "backups"), name);
  if (text === null) throw new Error(`No backup ${name}`);
  return text;
}

/** Writes a backup of `text` when the newest is older than the interval, then applies retention. */
export async function backUp(
  project: Dir,
  text: string,
  revision: number,
  now: number,
  policy: BackupPolicy,
): Promise<string | null> {
  const backups = await listBackups(project);
  const latest = backups[0];
  if (latest && now - latest.time < policy.interval) return null;
  const dir = await subdir(project, "backups");
  const name = backupName(now, revision);
  await writeText(dir, name, text);
  const keep = backupsToKeep([{ name, time: now, revision }, ...backups], now, policy);
  for (const backup of backups) if (!keep.has(backup.name)) await remove(dir, backup.name);
  return name;
}
