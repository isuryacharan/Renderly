// A project is a folder:
//
//   holiday-edit-d27a5e/
//     project.json        the edit
//     project.meta.json   name, duration, modified: all the home screen reads
//     backups/            rolling copies of project.json
//     media/              copied media (linked media stays where it is)
//     cache/              thumbnails, waveforms, proxies: safe to delete
//     packs/              pack items the project uses, so it opens offline

import {
  parseProject,
  sequenceDuration,
  serialiseProject,
  type MigrateResult,
  type Project,
} from "@renderly/core";
import { fileStamp, list, readText, remove, subdir, writeText, type Dir } from "./fs";

export const PROJECT_FILE = "project.json";
export const META_FILE = "project.meta.json";
export const FOLDERS = ["backups", "media", "cache", "packs"] as const;

export interface ProjectMeta {
  format: "renderly-meta";
  version: 1;
  id: string;
  name: string;
  /** Length of the first sequence, in flicks. */
  duration: number;
  /** When the project was last saved (ms since 1970). */
  modified: number;
  revision: number;
}

export function metaOf(project: Project, modified: number): ProjectMeta {
  const main = project.sequences[0];
  return {
    format: "renderly-meta",
    version: 1,
    id: project.id,
    name: project.name,
    duration: main ? sequenceDuration(main) : 0,
    modified,
    revision: project.revision,
  };
}

/** `Holiday edit!` with id `d27a5e…` → `holiday-edit-d27a5e`. */
export function folderName(name: string, id: string): string {
  const slug = name
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);
  return `${slug || "project"}-${id.slice(0, 6)}`;
}

export interface ProjectEntry {
  folder: string;
  meta: ProjectMeta;
}

/** Creates the project's folder with its layout and first save. Returns the folder name. */
export async function createProject(
  root: Dir,
  project: Project,
  now = Date.now(),
): Promise<string> {
  const folder = folderName(project.name, project.id);
  const dir = await subdir(root, folder);
  if ((await readText(dir, PROJECT_FILE)) !== null)
    throw new Error(`${folder} already holds a project`);
  for (const name of FOLDERS) await subdir(dir, name);
  await writeText(dir, PROJECT_FILE, serialiseProject(project));
  await writeText(dir, META_FILE, `${JSON.stringify(metaOf(project, now), null, 2)}\n`);
  return folder;
}

function parseMeta(text: string | null): ProjectMeta | null {
  if (text === null) return null;
  try {
    const meta = JSON.parse(text) as Partial<ProjectMeta>;
    return meta.format === "renderly-meta" && typeof meta.id === "string"
      ? (meta as ProjectMeta)
      : null;
  } catch {
    return null;
  }
}

/** Every project in a location, newest first. Folders without a readable meta file are skipped. */
export async function listProjects(root: Dir): Promise<ProjectEntry[]> {
  const found: ProjectEntry[] = [];
  for (const entry of await list(root)) {
    if (entry.kind !== "directory" || entry.name.startsWith(".")) continue;
    const meta = parseMeta(await readText(await subdir(root, entry.name, false), META_FILE));
    if (meta) found.push({ folder: entry.name, meta });
  }
  return found.sort((a, b) => b.meta.modified - a.meta.modified);
}

export interface Loaded {
  dir: Dir;
  /** The project file's text exactly as read, and its modification stamp. */
  text: string;
  stamp: number;
  result: MigrateResult;
}

export async function loadProject(root: Dir, folder: string): Promise<Loaded> {
  const dir = await subdir(root, folder, false);
  return loadFrom(dir);
}

export async function loadFrom(dir: Dir): Promise<Loaded> {
  const text = await readText(dir, PROJECT_FILE);
  if (text === null) throw new Error(`No ${PROJECT_FILE} in ${dir.name}`);
  return {
    dir,
    text,
    stamp: (await fileStamp(dir, PROJECT_FILE)) ?? 0,
    result: parseProject(text),
  };
}

export async function deleteProject(root: Dir, folder: string): Promise<void> {
  await remove(root, folder);
}
