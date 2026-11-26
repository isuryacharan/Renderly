// Test hooks for S1.6, on the editor's own origin so it shares browser storage with the app.
// Dev only: media-lab.html is not part of the build. scripts/media-e2e.mjs drives `window.mediaLab`.
//
// "Outside" files (a stand-in for the user's disk) live in browser storage under outside/, so the
// tests can move and delete them the way a person would in a file manager.

import { createId, newProject, type Command } from "@renderly/core";
import { createProber, type ProbeResult } from "@renderly/media";
import {
  browserRoot,
  createProject,
  importMedia,
  ProjectSession,
  relinkFromFolder,
  relinkMedia,
  resolveMedia,
  subdir,
} from "@renderly/storage";
import {
  AudioBufferSource,
  BufferTarget,
  CanvasSource,
  Output,
  QUALITY_LOW,
  WebMOutputFormat,
} from "mediabunny";

let session: ProjectSession | null = null;
const prober = createProber();
const probe = (file: Blob): Promise<ProbeResult> => prober.probe(file);

async function outside(path = ""): Promise<FileSystemDirectoryHandle> {
  let dir = await subdir(await navigator.storage.getDirectory(), "outside");
  for (const part of path.split("/").filter(Boolean)) dir = await subdir(dir, part);
  return dir;
}

async function outsideFile(path: string): Promise<FileSystemFileHandle> {
  const parts = path.split("/");
  return (await outside(parts.slice(0, -1).join("/"))).getFileHandle(parts.at(-1) ?? "");
}

function need(): ProjectSession {
  if (!session) throw new Error("No project open");
  return session;
}

const mediaLab = {
  /** Empties the projects and the outside folder. */
  async reset(): Promise<void> {
    await session?.close();
    session = null;
    const opfs = await navigator.storage.getDirectory();
    for (const name of ["projects", "outside"])
      await opfs.removeEntry(name, { recursive: true }).catch(() => undefined);
  },

  /** Encodes a short WebM (VP9 picture with a frame counter, Opus tone) into outside/<path>. */
  async makeClip(path: string, seconds = 1, label = path): Promise<number> {
    const canvas = new OffscreenCanvas(320, 180);
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("No 2D context");
    const target = new BufferTarget();
    const output = new Output({ format: new WebMOutputFormat(), target });
    // A keyframe every half second, so timeline filmstrips have distinct pictures to show.
    const video = new CanvasSource(canvas, {
      codec: "vp9",
      bitrate: QUALITY_LOW,
      keyFrameInterval: 0.5,
    });
    const audio = new AudioBufferSource({ codec: "opus", bitrate: QUALITY_LOW });
    output.addVideoTrack(video, { frameRate: 30 });
    output.addAudioTrack(audio);
    await output.start();
    const fps = 30;
    for (let i = 0; i < seconds * fps; i++) {
      ctx.fillStyle = `hsl(${(i * 7) % 360} 60% 40%)`;
      ctx.fillRect(0, 0, 320, 180);
      ctx.fillStyle = "white";
      ctx.font = "24px sans-serif";
      ctx.fillText(`${label} ${i}`, 16, 96);
      await video.add(i / fps, 1 / fps);
    }
    const rate = 48_000;
    const buffer = new AudioBuffer({
      length: rate * seconds,
      numberOfChannels: 1,
      sampleRate: rate,
    });
    const samples = buffer.getChannelData(0);
    for (let i = 0; i < samples.length; i++)
      samples[i] = 0.2 * Math.sin((2 * Math.PI * 440 * i) / rate);
    await audio.add(buffer);
    await output.finalize();
    const bytes = target.buffer;
    if (!bytes) throw new Error("Encoding produced nothing");
    const parts = path.split("/");
    const dir = await outside(parts.slice(0, -1).join("/"));
    const writable = await (
      await dir.getFileHandle(parts.at(-1) ?? "", { create: true })
    ).createWritable();
    await writable.write(bytes);
    await writable.close();
    return bytes.byteLength;
  },

  async writeGarbage(path: string): Promise<void> {
    const dir = await outside();
    const writable = await (await dir.getFileHandle(path, { create: true })).createWritable();
    await writable.write(new TextEncoder().encode("this is not a video ".repeat(1000)));
    await writable.close();
  },

  /** Moves an outside file the way a file manager does: the old place is gone. */
  async moveOutside(from: string, to: string): Promise<void> {
    const file = await (await outsideFile(from)).getFile();
    const parts = to.split("/");
    const dir = await outside(parts.slice(0, -1).join("/"));
    const writable = await (
      await dir.getFileHandle(parts.at(-1) ?? "", { create: true })
    ).createWritable();
    await file.stream().pipeTo(writable);
    const fromParts = from.split("/");
    await (await outside(fromParts.slice(0, -1).join("/"))).removeEntry(fromParts.at(-1) ?? "");
  },

  async createProject(name: string): Promise<string> {
    return createProject(await browserRoot(), newProject(name));
  },

  async open(folder: string): Promise<string> {
    await session?.close();
    session = await ProjectSession.open(await subdir(await browserRoot(), folder, false), {
      autosave: { debounce: 0 },
    });
    return session.role;
  },

  async close(): Promise<void> {
    await session?.close();
    session = null;
  },

  /** Imports outside files by handle (linked), or copied with copy: true. */
  async importOutside(paths: string[], copy = false) {
    const s = need();
    const sources = await Promise.all(
      paths.map(async (path) => ({ handle: await outsideFile(path) })),
    );
    const progress: number[] = [];
    const result = await importMedia(s, sources, probe, {
      copy,
      onProgress: (p) => p.copying && progress.push(p.copying.share),
    });
    await s.autosave?.flush();
    return {
      imported: result.imported.map((m) => ({
        id: m.id,
        name: m.name,
        link: m.link,
        path: m.path,
        video: m.video,
        audio: m.audio,
        duration: m.duration,
      })),
      failed: result.failed,
      progressSteps: progress.length,
      lastProgress: progress.at(-1) ?? null,
    };
  },

  /** Imports the files chosen in #files, measuring time and heap; linking is skipped (see below). */
  async importChosen() {
    const s = need();
    const input = document.getElementById("files") as HTMLInputElement;
    const files = [...(input.files ?? [])];
    const memory = () =>
      (performance as Performance & { memory?: { usedJSHeapSize: number } }).memory
        ?.usedJSHeapSize ?? 0;
    let peak = memory();
    const before = peak;
    const sampler = setInterval(() => (peak = Math.max(peak, memory())), 20);
    const started = performance.now();
    // Files from an <input> have no handle to keep; storing one is a single IndexedDB write per
    // file, so this measures everything else an import does: probe, fingerprint, thumbnail, command.
    const result = await importMedia(
      s,
      files.map((file) => ({ file })),
      probe,
      {
        copy: false,
        link: async () => undefined,
      },
    );
    const ms = performance.now() - started;
    clearInterval(sampler);
    peak = Math.max(peak, memory());
    return {
      ms,
      files: files.length,
      bytes: files.reduce((n, f) => n + f.size, 0),
      imported: result.imported.length,
      failed: result.failed,
      heapBeforeMB: before / 1e6,
      heapPeakMB: peak / 1e6,
    };
  },

  /** Adds `count` one-second clips of a media item on a new video track. */
  addClips(mediaId: string, count: number): string[] {
    const s = need();
    const sequence = s.store.project.sequences[0];
    if (!sequence) throw new Error("No sequence");
    const track = createId();
    const run = (command: Command) => {
      const result = s.store.dispatch(command);
      if (!result.ok) throw new Error(result.error.message);
    };
    run({ type: "track.add", payload: { sequence: sequence.id, id: track, kind: "video" } });
    const ids = Array.from({ length: count }, () => createId());
    run({
      type: "clip.add",
      payload: {
        track,
        clips: ids.map((id, i) => ({
          id,
          media: mediaId,
          start: i * 705_600_000,
          duration: 705_600_000 / 2,
        })),
      },
    });
    return ids;
  },

  /** Every media item and whether its file can be read now. */
  async statuses() {
    const s = need();
    return Promise.all(
      s.store.project.media.map(async (item) => {
        const status = await resolveMedia(s.dir, s.store.project.id, item);
        return {
          id: item.id,
          name: item.name,
          status: status.status,
          reason: status.status === "offline" ? status.reason : null,
        };
      }),
    );
  },

  /** Clips whose media can be read, and clips whose media cannot. */
  async clipStates() {
    const s = need();
    const states = new Map((await mediaLab.statuses()).map((m) => [m.id, m.status]));
    const clips = s.store.project.sequences.flatMap((q) => q.tracks.flatMap((t) => t.clips));
    const withMedia = clips.filter((c) => c.media !== undefined);
    return {
      online: withMedia.filter((c) => states.get(c.media ?? "") === "online").length,
      offline: withMedia.filter((c) => states.get(c.media ?? "") !== "online").length,
    };
  },

  async relinkFromOutside(folder: string): Promise<string[]> {
    const s = need();
    const offline = (await mediaLab.statuses())
      .filter((m) => m.status === "offline")
      .map((m) => m.id);
    const items = s.store.project.media.filter((m) => offline.includes(m.id));
    return relinkFromFolder(s.store.project.id, items, await outside(folder));
  },

  async relinkOne(mediaId: string, path: string) {
    const s = need();
    const item = s.store.project.media.find((m) => m.id === mediaId);
    if (!item) throw new Error("No such media");
    return relinkMedia(s.store.project.id, item, await outsideFile(path));
  },

  dispatch(command: Command) {
    return need().store.dispatch(command);
  },

  media() {
    return need().store.project.media.map((m) => ({ id: m.id, name: m.name }));
  },
};

Object.assign(window, { mediaLab, labReadyAt: performance.now() });
