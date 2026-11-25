// Media files for a project. A linked file stays where the user keeps it: the project holds its
// name and a fingerprint, and its handle is kept in IndexedDB. A copied file lives in the
// project's media/ folder. Either way, resolveMedia() says whether the file can be read now, and
// relinking points an offline item at its file again. Relinking changes no project data (clips
// refer to the media id), so every clip using the item comes back with it.

import type { MediaItem } from "@renderly/core";
import { remove, subdir, type Dir } from "./fs";
import { access, recallMediaHandle, rememberMediaHandle } from "./locations";

const HEAD_BYTES = 64 * 1024;

/** `sha256-<base64>` of the first 64 KiB: with the size, how a file is recognised. */
export async function fileHead(file: Blob): Promise<string> {
  const digest = new Uint8Array(
    await crypto.subtle.digest("SHA-256", await file.slice(0, HEAD_BYTES).arrayBuffer()),
  );
  let binary = "";
  for (const byte of digest) binary += String.fromCharCode(byte);
  return `sha256-${btoa(binary)}`;
}

const isNotFound = (error: unknown) =>
  error instanceof DOMException &&
  (error.name === "NotFoundError" || error.name === "NotReadableError");

// --- copying into the project ---

/** `clip.mp4`, then `clip (2).mp4`, `clip (3).mp4` ... whichever is free in `dir`. */
async function freeFileName(dir: Dir, name: string): Promise<string> {
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  for (let n = 1; ; n++) {
    const candidate = n === 1 ? name : `${stem} (${n})${ext}`;
    try {
      await dir.getFileHandle(candidate);
    } catch {
      return candidate;
    }
  }
}

/**
 * Streams a file into the project's media/ folder and returns its path in the project
 * (`media/clip.mp4`). `onProgress` gets bytes written so far. Never reads the file whole.
 */
export async function copyIntoProject(
  project: Dir,
  file: File,
  onProgress?: (written: number, total: number) => void,
): Promise<string> {
  const media = await subdir(project, "media");
  const name = await freeFileName(media, file.name);
  const writable = await (await media.getFileHandle(name, { create: true })).createWritable();
  let written = 0;
  const counter = new TransformStream<Uint8Array, Uint8Array>({
    transform(chunk, controller) {
      written += chunk.byteLength;
      onProgress?.(written, file.size);
      controller.enqueue(chunk);
    },
  });
  try {
    await file.stream().pipeThrough(counter).pipeTo(writable);
  } catch (error) {
    await remove(media, name);
    throw error;
  }
  return `media/${name}`;
}

/** Keeps a linked file's handle for this project. */
export const linkMedia = rememberMediaHandle;

// --- thumbnails (cache/thumbs: safe to delete, rebuilt on import) ---

export async function saveThumbnail(project: Dir, mediaId: string, jpeg: Blob): Promise<void> {
  const thumbs = await subdir(await subdir(project, "cache"), "thumbs");
  const writable = await (
    await thumbs.getFileHandle(`${mediaId}.jpg`, { create: true })
  ).createWritable();
  await writable.write(jpeg);
  await writable.close();
}

export async function readThumbnail(project: Dir, mediaId: string): Promise<File | null> {
  try {
    const thumbs = await subdir(await subdir(project, "cache"), "thumbs");
    return await (await thumbs.getFileHandle(`${mediaId}.jpg`)).getFile();
  } catch {
    return null;
  }
}

// --- resolving ---

export type OfflineReason =
  /** The file is not where it was: moved, renamed or deleted. */
  | "missing"
  /** The browser needs the user's permission again (after a restart): one click reconnects. */
  | "permission"
  /** A file is there, but it is not the one imported. */
  | "changed"
  /** No handle was kept (another browser, or cleared site data). */
  | "unlinked";

export type MediaStatus =
  { status: "online"; file: File } | { status: "offline"; reason: OfflineReason };

/** Whether an item's file can be read now, without asking the user anything. */
export async function resolveMedia(
  project: Dir,
  projectId: string,
  item: MediaItem,
): Promise<MediaStatus> {
  let file: File;
  try {
    if (item.link === "copy") {
      const parts = item.path.split("/");
      let dir = project;
      for (const part of parts.slice(0, -1)) dir = await subdir(dir, part, false);
      file = await (await dir.getFileHandle(parts.at(-1) ?? "")).getFile();
    } else {
      const handle = await recallMediaHandle(projectId, item.id);
      if (!handle) return { status: "offline", reason: "unlinked" };
      if ((await access(handle, false, "read")) !== "granted")
        return { status: "offline", reason: "permission" };
      file = await handle.getFile();
    }
  } catch (error) {
    if (isNotFound(error) || error instanceof TypeError)
      return { status: "offline", reason: "missing" };
    throw error;
  }
  return (await matches(file, item))
    ? { status: "online", file }
    : { status: "offline", reason: "changed" };
}

/** Size and head must match; the head is only read when the size and date alone do not settle it. */
async function matches(file: File, item: MediaItem): Promise<boolean> {
  if (file.size !== item.fingerprint.size) return false;
  if (file.lastModified === item.fingerprint.modified) return true;
  return (await fileHead(file)) === item.fingerprint.head;
}

/**
 * Asks once for permission to read every linked file that needs it (after a browser restart).
 * Call from a click: the browser shows its prompt for each handle in turn.
 */
export async function reconnectMedia(
  projectId: string,
  items: readonly MediaItem[],
): Promise<number> {
  let granted = 0;
  for (const item of items) {
    if (item.link !== "handle") continue;
    const handle = await recallMediaHandle(projectId, item.id);
    if (handle && (await access(handle, true, "read")) === "granted") granted++;
  }
  return granted;
}

// --- relinking ---

export type RelinkResult = { ok: true } | { ok: false; reason: "different-file" };

/** Points an item at a file the user picked, if it is the same file (size and head). */
export async function relinkMedia(
  projectId: string,
  item: MediaItem,
  handle: FileSystemFileHandle,
): Promise<RelinkResult> {
  const file = await handle.getFile();
  if (file.size !== item.fingerprint.size || (await fileHead(file)) !== item.fingerprint.head)
    return { ok: false, reason: "different-file" };
  await rememberMediaHandle(projectId, item.id, handle);
  return { ok: true };
}

/**
 * Looks through a folder (and its subfolders) for the files of offline items, matching by size
 * first and then by head, and relinks every one it finds. Returns the ids relinked.
 */
export async function relinkFromFolder(
  projectId: string,
  items: readonly MediaItem[],
  folder: Dir,
): Promise<string[]> {
  const bySize = new Map<number, MediaItem[]>();
  for (const item of items)
    bySize.set(item.fingerprint.size, [...(bySize.get(item.fingerprint.size) ?? []), item]);
  const relinked: string[] = [];
  const walk = async (dir: Dir): Promise<void> => {
    for await (const entry of dir.values()) {
      if (relinked.length === items.length) return;
      if (entry.kind === "directory") {
        await walk(entry);
        continue;
      }
      const file = await entry.getFile();
      const candidates = bySize.get(file.size)?.filter((i) => !relinked.includes(i.id));
      if (!candidates?.length) continue;
      const head = await fileHead(file);
      for (const item of candidates) {
        if (item.fingerprint.head !== head) continue;
        await rememberMediaHandle(projectId, item.id, entry);
        relinked.push(item.id);
      }
    }
  };
  await walk(folder);
  return relinked;
}
