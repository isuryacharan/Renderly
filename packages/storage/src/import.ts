// Importing media into an open project: probe each file in the worker pool, link it by handle
// (or copy it into the project), keep its thumbnail, then add every item with one media.import
// command, so the whole import is one undo step. Only headers, a 64 KiB fingerprint and one
// thumbnail frame are read; a linked file is never copied or loaded.

import {
  createId,
  MEDIA_SAMPLE_RATES,
  nearestMediaRate,
  type MediaItem,
  type ProjectStore,
} from "@renderly/core";
import type { ProbeResult } from "@renderly/media";
import type { Dir } from "./fs";
import { copyIntoProject, linkMedia, saveThumbnail } from "./media";

/** A file to import: from a picker or a drop with a handle (linkable), or a bare File. */
export interface ImportSource {
  file?: File;
  handle?: FileSystemFileHandle;
}

export interface ImportOptions {
  /** Copy files into the project's media/ folder instead of linking them. */
  copy: boolean;
  onProgress?: (progress: ImportProgress) => void;
  /** Overrides how a file is linked; tests use it to measure import without handles. */
  link?: (mediaId: string, source: ImportSource, file: File) => Promise<void>;
}

export interface ImportProgress {
  /** Files finished (imported or failed) and in total. */
  done: number;
  total: number;
  /** While copying: the file and how far along it is (0 to 1). */
  copying?: { name: string; share: number };
}

export interface ImportResult {
  imported: MediaItem[];
  failed: { name: string; reason: string }[];
}

/** Video and audio file extensions picked up when importing a whole folder. */
export const MEDIA_EXTENSIONS = [
  "mp4",
  "m4v",
  "mov",
  "webm",
  "mkv",
  "avi",
  "mts",
  "m2ts",
  "ts",
  "mp3",
  "m4a",
  "aac",
  "wav",
  "flac",
  "ogg",
  "oga",
  "opus",
  "aif",
  "aiff",
];

const isMediaName = (name: string) =>
  MEDIA_EXTENSIONS.includes(name.split(".").pop()?.toLowerCase() ?? "");

/** Every media file in a folder and its subfolders, as handles. */
export async function mediaInFolder(folder: Dir): Promise<FileSystemFileHandle[]> {
  const found: FileSystemFileHandle[] = [];
  const walk = async (dir: Dir) => {
    for await (const entry of dir.values()) {
      if (entry.kind === "directory") await walk(entry);
      else if (isMediaName(entry.name)) found.push(entry);
    }
  };
  await walk(folder);
  return found.sort((a, b) => a.name.localeCompare(b.name));
}

const CODEC_NAMES: Record<string, string> = {
  avc: "H.264",
  hevc: "HEVC",
  vp8: "VP8",
  vp9: "VP9",
  av1: "AV1",
  aac: "AAC",
  opus: "Opus",
  mp3: "MP3",
  vorbis: "Vorbis",
  flac: "FLAC",
  ac3: "AC-3",
  eac3: "E-AC-3",
};

/** "avc" → "H.264", "pcm-s16" → "PCM". */
export function codecName(codec: string): string {
  if (codec.startsWith("pcm")) return "PCM";
  return CODEC_NAMES[codec] ?? codec.toUpperCase();
}

/** The project's record of a probed file. Rates are snapped to the ones a source may have. */
export function mediaItem(
  id: string,
  file: File,
  probe: Extract<ProbeResult, { ok: true }>,
  link: { link: "handle" | "copy"; path: string },
): MediaItem {
  const audioRate = probe.audio?.sampleRate ?? 0;
  return {
    id,
    name: file.name,
    link: link.link,
    path: link.path,
    fingerprint: { size: file.size, modified: file.lastModified, head: probe.head },
    duration: probe.duration,
    ...(probe.video
      ? {
          video: {
            width: Math.max(1, probe.video.width),
            height: Math.max(1, probe.video.height),
            fps: [...nearestMediaRate(probe.video.fps > 0 ? probe.video.fps : 30)] as [
              number,
              number,
            ],
            codec: probe.video.codec,
            rotation: probe.video.rotation,
          },
        }
      : {}),
    // Audio at a rate no flick-exact clock can hold is left out rather than refused.
    ...(probe.audio && (MEDIA_SAMPLE_RATES as readonly number[]).includes(audioRate)
      ? {
          audio: {
            sampleRate: audioRate,
            channels: Math.min(32, Math.max(1, probe.audio.channels)),
            codec: probe.audio.codec,
          },
        }
      : {}),
  };
}

export async function importMedia(
  project: { dir: Dir; store: ProjectStore },
  sources: readonly ImportSource[],
  probe: (file: Blob) => Promise<ProbeResult>,
  options: ImportOptions,
): Promise<ImportResult> {
  const { dir, store } = project;
  if (store.readOnly) throw new Error("The project is open read-only in this tab");
  const projectId = store.project.id;
  const result: ImportResult = { imported: [], failed: [] };
  let done = 0;
  const report = (copying?: ImportProgress["copying"]) =>
    options.onProgress?.({ done, total: sources.length, ...(copying ? { copying } : {}) });

  const one = async (source: ImportSource): Promise<MediaItem | null> => {
    const file = source.handle ? await source.handle.getFile() : source.file;
    if (!file) return null;
    const probed = await probe(file);
    if (!probed.ok) {
      result.failed.push({ name: file.name, reason: probed.detail });
      return null;
    }
    const id = createId();
    let link: { link: "handle" | "copy"; path: string };
    if (options.link) {
      await options.link(id, source, file);
      link = { link: "handle", path: file.name };
    } else if (options.copy || !source.handle) {
      const path = await copyIntoProject(dir, file, (written, total) =>
        report({ name: file.name, share: total > 0 ? written / total : 1 }),
      );
      link = { link: "copy", path };
    } else {
      await linkMedia(projectId, id, source.handle);
      link = { link: "handle", path: file.name };
    }
    if (probed.thumbnail) await saveThumbnail(dir, id, probed.thumbnail).catch(() => undefined);
    return mediaItem(id, file, probed, link);
  };

  // Copies go one at a time (they are disk-bound); probes overlap in the worker pool.
  const items: (MediaItem | null)[] = [];
  if (options.copy) {
    for (const source of sources) {
      items.push(await one(source));
      done++;
      report();
    }
  } else {
    items.push(
      ...(await Promise.all(
        sources.map(async (source) => {
          const item = await one(source).catch((error: unknown) => {
            result.failed.push({
              name: source.handle?.name ?? source.file?.name ?? "file",
              reason: error instanceof Error ? error.message : String(error),
            });
            return null;
          });
          done++;
          report();
          return item;
        }),
      )),
    );
  }

  result.imported = items.filter((item): item is MediaItem => item !== null);
  if (result.imported.length > 0) {
    const dispatched = store.dispatch({
      type: "media.import",
      payload: { items: result.imported },
    });
    if (!dispatched.ok) throw new Error(dispatched.error.message);
  }
  return result;
}
