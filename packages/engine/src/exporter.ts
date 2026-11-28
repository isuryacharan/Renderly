// The export worker: decodes the edit list with its own decoders, composites every output frame
// with the same uploader and compositor as the preview, encodes with VideoEncoder / AudioEncoder
// (through mediabunny) and streams the MP4 to a writable file. Memory stays flat: frames are
// produced one at a time with encoder back-pressure, audio goes through a small ring in fixed
// chunks, and the file is written as it grows (the index goes at the end).
//
// Jobs follow the shared job protocol: progress, cancel, typed result.

import { AudioRing, OUTPUT_SAMPLE_RATE, serveAudioDecoder } from "@renderly/audio";
import {
  DecodeClient,
  FLICKS_PER_SECOND,
  type DecodedFrame,
  type Endpoint,
  type MediaInput,
  type OpenResult,
} from "@renderly/media";
import {
  AudioSample,
  AudioSampleSource,
  Mp4OutputFormat,
  Output,
  StreamTarget,
  VideoSample,
  VideoSampleSource,
} from "mediabunny";
import type { BackgroundSettings } from "@renderly/core";
import * as THREE from "three";
import { BLACK_BACKGROUND } from "./blend";
import { Compositor } from "./compositor";
import { FrameQueue } from "./frame-queue";
import { FrameUploader } from "./frame-upload";
import { containRect } from "./layout";
import { MemoryBudget, decodedFrameBytes } from "./memory";
import { Presenter } from "./present";
import {
  runIdAt,
  sourceTime,
  timelineTime,
  toTimeline,
  wantedRuns,
  type Cut,
  type Run,
  type Timeline,
  runTransformAt,
} from "./timeline";

export interface ExportSettings {
  width: number;
  height: number;
  /** Whole frames per second; FLICKS_PER_SECOND must be divisible by it (24, 25, 30, 50, 60). */
  fps: number;
  videoBitrate: number;
  audioBitrate: number;
  /** What fills the frame behind the clips; black when left out. */
  background?: BackgroundSettings;
}

export type ExportResult =
  | {
      ok: true;
      frames: number;
      seconds: number;
      heapStartMB: number | null;
      heapPeakMB: number | null;
      gpuPeakMB: number;
    }
  | { ok: false; reason: "cancelled" }
  | { ok: false; reason: "failed"; message: string };

export type ToExporter =
  | { type: "init"; decoders: MessagePort[] }
  | {
      type: "start";
      id: number;
      file: FileSystemFileHandle;
      sources: MediaInput[];
      cuts: Cut[];
      settings: ExportSettings;
    }
  | { type: "cancel"; id: number }
  /** Parity test: composite one frame exactly as export would, and return its pixels. */
  | {
      type: "render-frame";
      id: number;
      sources: MediaInput[];
      cuts: Cut[];
      settings: ExportSettings;
      time: number;
    };

export type FromExporter =
  | {
      type: "progress";
      id: number;
      frames: number;
      total: number;
      heapMB: number | null;
      gpuMB: number;
    }
  | { type: "done"; id: number; result: ExportResult }
  | { type: "frame"; id: number; pixels: Uint8Array; width: number; height: number }
  | { type: "error"; id: number; message: string };

const PROGRESS_INTERVAL_MS = 500;
const AUDIO_CHUNK = 1024;
const AUDIO_RING_SECONDS = 2;
const EXPORT_GPU_CEILING = 1536 * 1024 * 1024;

interface Slot {
  client: DecodeClient;
  source: number;
  busy: boolean;
}

interface TimelineFrame {
  decoded: DecodedFrame;
  time: number;
  duration: number;
}

interface Stream {
  run: number;
  slot: Slot;
  queue: FrameQueue<TimelineFrame>;
  ended: boolean;
  stopped: boolean;
  stop: () => void;
  /** Wakes whoever waits for the next frame or the end. */
  wake: (() => void) | null;
}

/** JS heap of this worker in MB, where the browser reports it. */
function heapMB(): number | null {
  const memory = (performance as { memory?: { usedJSHeapSize: number } }).memory;
  return memory ? Math.round((memory.usedJSHeapSize / 1048576) * 10) / 10 : null;
}

export function serveExporter(endpoint: Endpoint): void {
  let slots: Slot[] = [];
  let cancelled = 0;
  const post = (message: FromExporter, transfer: Transferable[] = []) =>
    endpoint.postMessage(message, transfer);

  /** One export or parity render: its GL context, decoders and timeline. */
  class Job {
    readonly timeline: Timeline;
    readonly budget = new MemoryBudget(EXPORT_GPU_CEILING);
    readonly canvas: OffscreenCanvas;
    readonly renderer: THREE.WebGLRenderer;
    readonly uploader: FrameUploader;
    readonly compositor: Compositor;
    readonly presenter: Presenter;
    readonly streams = new Map<number, Stream>();
    readonly infos: (Extract<OpenResult, { ok: true }> | null)[] = [];
    /** Per layer index: the run and frame interval in its target. */
    readonly shown: ({ run: number; time: number; duration: number } | null)[];

    constructor(
      readonly sources: MediaInput[],
      cuts: Cut[],
      readonly settings: ExportSettings,
    ) {
      this.timeline = toTimeline(cuts);
      this.shown = this.timeline.layers.map(() => null);
      this.canvas = new OffscreenCanvas(settings.width, settings.height);
      this.renderer = new THREE.WebGLRenderer({
        canvas: this.canvas,
        antialias: false,
        alpha: false,
        depth: false,
        stencil: false,
        preserveDrawingBuffer: true,
        powerPreference: "high-performance",
      });
      this.renderer.setPixelRatio(1);
      this.renderer.autoClear = false;
      this.renderer.setSize(settings.width, settings.height, false);
      this.uploader = new FrameUploader(this.renderer);
      this.compositor = new Compositor(this.renderer, this.budget);
      this.compositor.resize({ width: settings.width, height: settings.height });
      this.compositor.setBackground(settings.background ?? BLACK_BACKGROUND);
      this.presenter = new Presenter(this.renderer);
    }

    async openSources(): Promise<void> {
      // The first decoder opens every source once to learn sizes and rotation.
      const slot = slots[0];
      if (!slot) throw new Error("Exporter has no decoders");
      for (const [i, input] of this.sources.entries()) {
        const result = await slot.client.open(input);
        this.infos[i] = result.ok ? result : null;
        if (!result.ok) throw new Error(`Cannot open source ${i}: ${result.reason}`);
      }
      slot.source = this.sources.length - 1;
    }

    private idleSlot(source: number): Slot | null {
      const idle = slots.filter((slot) => !slot.busy);
      return idle.find((slot) => slot.source === source) ?? idle[0] ?? null;
    }

    /** Keeps streams on the runs at `t` and the next ones; retires runs already finished. */
    schedule(t: number): void {
      const { current, ahead } = wantedRuns(this.timeline, t, {
        minAhead: 1,
        lookahead: 0,
        maxStreams: slots.length,
      });
      const wanted = new Set([...current, ...ahead]);
      for (const stream of [...this.streams.values()]) {
        if (!wanted.has(stream.run)) this.retire(stream);
      }
      for (const id of [...current, ...ahead]) this.ensure(id, t);
    }

    private ensure(id: number, t: number): void {
      const run = this.timeline.runs[id];
      if (!run || this.streams.has(id)) return;
      const slot = this.idleSlot(run.source);
      if (!slot) return;
      slot.busy = true;
      const stream: Stream = {
        run: id,
        slot,
        queue: new FrameQueue<TimelineFrame>(),
        ended: false,
        stopped: false,
        stop: () => {},
        wake: null,
      };
      this.streams.set(id, stream);
      const from = t > run.start ? sourceTime(run, t) : run.in;
      void this.play(stream, run, from).catch((error: unknown) => {
        stream.ended = true;
        stream.wake?.();
        post({ type: "error", id: 0, message: `Run ${id}: ${String(error)}` });
      });
    }

    private async play(stream: Stream, run: Run, from: number): Promise<void> {
      const { slot } = stream;
      if (slot.source !== run.source) {
        const result = await slot.client.open(this.sources[run.source] as MediaInput);
        if (!result.ok) throw new Error(`Cannot open source ${run.source}`);
        slot.source = run.source;
      }
      if (stream.stopped) return;
      if (run.freeze) {
        // A freeze frame: one exact frame, shown for the whole run.
        const decoded = await slot.client.seek(run.in, "exact");
        if (decoded && !stream.stopped)
          stream.queue.push({ decoded, time: run.start, duration: run.end - run.start });
        else decoded?.release();
        stream.ended = true;
        stream.wake?.();
        slot.busy = false;
        return;
      }
      const playback = slot.client.play(from, run.out, (decoded) => {
        if (stream.stopped) {
          decoded.release();
          return;
        }
        stream.queue.push({
          decoded,
          time: timelineTime(run, decoded.time),
          duration: decoded.duration,
        });
        stream.wake?.();
      });
      stream.stop = playback.stop;
      await playback.done;
      stream.ended = true;
      stream.wake?.();
      slot.busy = false;
    }

    private retire(stream: Stream): void {
      stream.stopped = true;
      stream.stop();
      for (const frame of stream.queue.clear()) frame.decoded.release();
      stream.slot.busy = false;
      this.streams.delete(stream.run);
      stream.wake?.();
    }

    /**
     * Waits until the frame showing at `t` on run `id` is certain (a later frame has arrived, or
     * the stream ended), then returns it, or null if the frame already uploaded still covers `t`.
     */
    private async frameAt(id: number, t: number): Promise<TimelineFrame | null> {
      for (;;) {
        const stream = this.streams.get(id);
        if (!stream) {
          this.schedule(t);
          if (!this.streams.get(id)) await new Promise((resolve) => setTimeout(resolve, 2));
          continue;
        }
        const later = hasLater(stream.queue, t);
        if (later || stream.ended) {
          const pick = stream.queue.take(t);
          for (const frame of pick.dropped) frame.decoded.release();
          return pick.frame;
        }
        await new Promise<void>((resolve) => {
          stream.wake = resolve;
          setTimeout(resolve, 50);
        });
      }
    }

    /** Composites output frame time `t` into the canvas. */
    async render(t: number): Promise<void> {
      this.schedule(t);
      for (const [i, { layer }] of this.timeline.layers.entries()) {
        const id = runIdAt(this.timeline, i, t);
        if (id < 0) {
          this.shown[i] = null;
          continue;
        }
        const frame = await this.frameAt(id, t);
        if (!frame) continue; // the uploaded frame still covers t
        try {
          const run = this.timeline.runs[id] as Run;
          const info = this.infos[run.source];
          const target = this.compositor.layerTarget(layer);
          if (info && target)
            this.compositor.setPictureBox(
              layer,
              this.uploader.upload(frame.decoded.frame, info.rotation, target, run.transform.fit),
            );
          this.shown[i] = { run: id, time: frame.time, duration: frame.duration };
        } finally {
          frame.decoded.release();
        }
      }
      const draws = this.timeline.layers.flatMap(({ layer }, i) => {
        const shown = this.shown[i];
        const id = runIdAt(this.timeline, i, t);
        if (!shown || id < 0) return [];
        return [{ layer, transform: runTransformAt(this.timeline.runs[id] as Run, t) }];
      });
      this.compositor.composite(draws);
      const texture = this.compositor.texture;
      const { width, height } = this.settings;
      if (texture)
        this.presenter.present(texture, containRect({ width, height }, { width, height }));
      let frames = 0;
      for (const slot of slots) {
        const info = this.infos[slot.source];
        if (info) frames += slot.client.outstanding * decodedFrameBytes(info.width, info.height);
      }
      this.budget.set("frames", frames);
    }

    dispose(): void {
      for (const stream of [...this.streams.values()]) this.retire(stream);
      this.compositor.dispose();
      this.uploader.dispose();
      this.presenter.dispose();
      this.renderer.dispose();
    }
  }

  /** True when the queue holds a frame starting after `t`. */
  function hasLater(queue: FrameQueue<TimelineFrame>, t: number): boolean {
    return queue.lastTime() > t;
  }

  async function exportJob(
    id: number,
    file: FileSystemFileHandle,
    sources: MediaInput[],
    cuts: Cut[],
    settings: ExportSettings,
  ): Promise<ExportResult> {
    const startedAt = performance.now();
    const heapStart = heapMB();
    let heapPeak = heapStart;
    const job = new Job(sources, cuts, settings);
    const writable = await file.createWritable();
    const output = new Output({
      format: new Mp4OutputFormat({ fastStart: false }),
      target: new StreamTarget(writable as unknown as WritableStream),
    });
    const video = new VideoSampleSource({
      codec: "avc",
      bitrate: settings.videoBitrate,
      keyFrameInterval: 2,
    });
    output.addVideoTrack(video, { frameRate: settings.fps });

    // Audio: the bottom layer's cuts, decoded by the same code as playback into a small ring.
    const audioCuts = cuts.filter((cut) => (cut.layer ?? 0) === 0 && !cut.muted);
    const audioChannel = new MessageChannel();
    serveAudioDecoder(audioChannel.port1 as unknown as Endpoint);
    const audioPort = audioChannel.port2;
    const loaded = new Promise<{ ok: boolean; channels?: number }[]>((resolve) => {
      audioPort.onmessage = (
        event: MessageEvent<{ type: string; results?: { ok: boolean; channels?: number }[] }>,
      ) => {
        if (event.data.type === "loaded") resolve(event.data.results ?? []);
      };
    });
    audioPort.postMessage({ type: "load", id: 1, sources });
    const audioResults = await loaded;
    const channels = Math.max(1, ...audioResults.map((r) => (r.ok ? (r.channels ?? 1) : 1)));
    const hasAudio = audioResults.some((r) => r.ok);
    let audio: AudioSampleSource | null = null;
    let ring: AudioRing | null = null;
    if (hasAudio) {
      audio = new AudioSampleSource({ codec: "aac", bitrate: settings.audioBitrate });
      output.addAudioTrack(audio);
      ring = AudioRing.create(channels, AUDIO_RING_SECONDS * OUTPUT_SAMPLE_RATE);
      audioPort.postMessage({
        type: "fill",
        session: 1,
        ring: ring.buffer,
        channels,
        capacity: ring.capacity,
        from: 0,
        cuts: audioCuts,
      });
    }

    const frameFlicks = FLICKS_PER_SECOND / settings.fps;
    const total = Math.ceil(job.timeline.duration / frameFlicks);
    const totalSamples = Math.round(
      (job.timeline.duration * OUTPUT_SAMPLE_RATE) / FLICKS_PER_SECOND,
    );
    let samplesWritten = 0;
    const planes = Array.from({ length: channels }, () => new Float32Array(AUDIO_CHUNK));
    const isCancelled = () => cancelled === id;

    /** Encodes audio up to timeline sample `until`, in fixed chunks. */
    const writeAudioUntil = async (until: number) => {
      if (!audio || !ring) return;
      while (samplesWritten < until && !isCancelled()) {
        const want = Math.min(AUDIO_CHUNK, until - samplesWritten);
        while (ring.readable < want && !ring.complete && !isCancelled()) {
          await new Promise((resolve) => setTimeout(resolve, 2));
        }
        const got = ring.read(planes, 0, Math.min(want, ring.readable));
        const count = got > 0 ? got : want; // past the end of the audio: silence
        if (got === 0) for (const plane of planes) plane.fill(0, 0, count);
        const data = new Float32Array(count * channels);
        for (let c = 0; c < channels; c++)
          data.set((planes[c] as Float32Array).subarray(0, count), c * count);
        const sample = new AudioSample({
          data,
          format: "f32-planar",
          numberOfChannels: channels,
          sampleRate: OUTPUT_SAMPLE_RATE,
          timestamp: samplesWritten / OUTPUT_SAMPLE_RATE,
        });
        await audio.add(sample);
        sample.close();
        samplesWritten += count;
      }
    };

    let lastProgress = 0;
    try {
      await job.openSources();
      await output.start();
      for (let k = 0; k < total; k++) {
        if (isCancelled()) break;
        const t = k * frameFlicks;
        await job.render(t);
        const frame = new VideoFrame(job.canvas, {
          timestamp: Math.round((k * 1e6) / settings.fps),
          duration: Math.round(1e6 / settings.fps),
        });
        const sample = new VideoSample(frame);
        await video.add(sample);
        sample.close();
        await writeAudioUntil(
          Math.min(totalSamples, Math.round(((k + 1) * OUTPUT_SAMPLE_RATE) / settings.fps)),
        );
        const heap = heapMB();
        if (heap !== null) heapPeak = Math.max(heapPeak ?? heap, heap);
        if (performance.now() - lastProgress > PROGRESS_INTERVAL_MS) {
          lastProgress = performance.now();
          post({
            type: "progress",
            id,
            frames: k + 1,
            total,
            heapMB: heap,
            gpuMB: Math.round(job.budget.used / 1048576),
          });
        }
      }
      if (isCancelled()) {
        await output.cancel();
        await removeFile(file);
        return { ok: false, reason: "cancelled" };
      }
      await writeAudioUntil(totalSamples);
      await output.finalize();
      return {
        ok: true,
        frames: total,
        seconds: Math.round((performance.now() - startedAt) / 100) / 10,
        heapStartMB: heapStart,
        heapPeakMB: heapPeak,
        gpuPeakMB: Math.round(job.budget.peak / 1048576),
      };
    } catch (error) {
      try {
        await output.cancel();
      } catch {
        // already failed
      }
      await removeFile(file);
      return {
        ok: false,
        reason: "failed",
        message: error instanceof Error ? error.message : String(error),
      };
    } finally {
      audioPort.postMessage({ type: "stop" });
      audioPort.close();
      job.dispose();
    }
  }

  /** Removes a partial file after a cancel or failure. */
  async function removeFile(file: FileSystemFileHandle): Promise<void> {
    try {
      await (file as FileSystemFileHandle & { remove?: () => Promise<void> }).remove?.();
    } catch {
      // nothing more to do
    }
  }

  async function renderFrame(
    sources: MediaInput[],
    cuts: Cut[],
    settings: ExportSettings,
    time: number,
  ) {
    const job = new Job(sources, cuts, settings);
    try {
      await job.openSources();
      await job.render(time);
      const pixels = job.compositor.readPixels();
      if (!pixels) throw new Error("No composite");
      return pixels;
    } finally {
      job.dispose();
    }
  }

  endpoint.onmessage = (event: MessageEvent<ToExporter>) => {
    const message = event.data;
    switch (message.type) {
      case "init":
        slots = message.decoders.map((port) => ({
          client: new DecodeClient(port),
          source: -1,
          busy: false,
        }));
        return;
      case "start":
        exportJob(message.id, message.file, message.sources, message.cuts, message.settings).then(
          (result) => post({ type: "done", id: message.id, result }),
          (error: unknown) =>
            post({
              type: "done",
              id: message.id,
              result: { ok: false, reason: "failed", message: String(error) },
            }),
        );
        return;
      case "cancel":
        cancelled = message.id;
        return;
      case "render-frame":
        renderFrame(message.sources, message.cuts, message.settings, message.time).then(
          (pixels) =>
            post(
              {
                type: "frame",
                id: message.id,
                pixels,
                width: message.settings.width,
                height: message.settings.height,
              },
              [pixels.buffer],
            ),
          (error: unknown) => post({ type: "error", id: message.id, message: String(error) }),
        );
        return;
    }
  };
}
