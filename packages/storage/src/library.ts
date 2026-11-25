// What the home screen does to projects: rename, duplicate, delete to a recycle bin, restore and
// delete for good. A rename goes through a session and a command, like every edit, so a project
// open in another tab is never changed behind that tab's back.

import { createId, parseProject, type Project } from "@renderly/core";
import { copyDir, freeName, list, moveDir, readText, remove, subdir, type Dir } from "./fs";
import { createProject, listProjects, PROJECT_FILE, type ProjectEntry } from "./projects";
import { ProjectSession } from "./session";

/** Deleted projects wait here until restored or deleted for good. */
export const RECYCLE = ".recycle";

export type RenameResult = "renamed" | "open-elsewhere";

export async function renameProject(
  root: Dir,
  folder: string,
  name: string,
): Promise<RenameResult> {
  const session = await ProjectSession.open(await subdir(root, folder, false), {
    autosave: { debounce: 0 },
  });
  try {
    if (session.role !== "writer") return "open-elsewhere";
    const result = session.store.dispatch({ type: "project.rename", payload: { name } });
    if (!result.ok) throw new Error(result.error.message);
    return "renamed";
  } finally {
    await session.close();
  }
}

async function readProject(dir: Dir): Promise<Project> {
  const result = parseProject((await readText(dir, PROJECT_FILE)) ?? "");
  if (!result.ok) throw new Error(`${dir.name} does not hold a valid project`);
  return result.project;
}

/**
 * Copies a project under a new id and name, with its copied media, cache and packs. Backups stay
 * with the original. Returns the new folder.
 */
export async function duplicateProject(root: Dir, folder: string, name: string): Promise<string> {
  const source = await subdir(root, folder, false);
  const project = await readProject(source);
  const copy = await createProject(root, { ...project, id: createId(), name, revision: 0 });
  const target = await subdir(root, copy, false);
  for (const part of ["media", "cache", "packs"]) {
    await copyDir(await subdir(source, part), await subdir(target, part));
  }
  return copy;
}

/** Moves a project to the recycle bin. */
export async function recycleProject(root: Dir, folder: string): Promise<void> {
  const bin = await subdir(root, RECYCLE);
  await moveDir(root, folder, bin, await freeName(bin, folder));
}

export async function listRecycled(root: Dir): Promise<ProjectEntry[]> {
  return listProjects(await subdir(root, RECYCLE));
}

/** Moves a project back out of the recycle bin. Returns its folder. */
export async function restoreProject(root: Dir, folder: string): Promise<string> {
  const bin = await subdir(root, RECYCLE);
  const name = await freeName(root, folder);
  await moveDir(bin, folder, root, name);
  return name;
}

/** Deletes a recycled project and everything in its folder. Cannot be undone. */
export async function purgeProject(root: Dir, folder: string): Promise<void> {
  await remove(await subdir(root, RECYCLE), folder);
}

/** Every file name in a project folder, recursively (for tests and the "on disk" view). */
export async function listFiles(dir: Dir, prefix = ""): Promise<string[]> {
  const names: string[] = [];
  for (const entry of await list(dir)) {
    const path = `${prefix}${entry.name}`;
    if (entry.kind === "directory")
      names.push(...(await listFiles(await subdir(dir, entry.name, false), `${path}/`)));
    else names.push(path);
  }
  return names;
}
