// S0.2 measurement page. Results are logged on the page and kept on `window.benchResults` so a
// test driver can read them.

import "@renderly/ui/fonts.css";
import "@renderly/ui/styles.css";
import "./spike.css";
import {
  createDecodeClient,
  flicksToSeconds,
  type DecodedFrame,
  type Flicks,
  type MediaInput,
} from "@renderly/media";

interface Results {
  open?: { file: string; bytes: number; ms: number; heapMB: number | null; info: unknown };
  seek?: {
    mode: string;
    count: number;
    medianMs: number;
    p90Ms: number;
    maxMs: number;
    wrong: number;
  };
  soak?: {
    seconds: number;
    plays: number;
    seeks: number;
    stops: number;
    frames: number;
    orderErrors: number;
    outstanding: number;
    workerInFlight: number;
    workerFramesHeld: number;
    heapStartMB: number | null;
    heapEndMB: number | null;
  };
}

const results: Results = {};
Object.assign(window, { benchResults: results });

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const mediaSelect = $<HTMLSelectElement>("media");
const fileInput = $<HTMLInputElement>("file");
const soakSeconds = $<HTMLInputElement>("soak-seconds");
const view = $<HTMLCanvasElement>("view");
const logEl = $<HTMLPreElement>("log");
const ctx = view.getContext("2d");

const client = createDecodeClient();
let duration: Flicks = 0;
let busy = false;

function log(line: string): void {
  logEl.textContent += `${line}\n`;
}

/** Main-thread JS heap in MB (Chrome only). */
function heapMB(): number | null {
  const memory = (performance as { memory?: { usedJSHeapSize: number } }).memory;
  return memory ? Math.round((memory.usedJSHeapSize / 1048576) * 10) / 10 : null;
}

/** Deterministic random numbers so runs are comparable. */
function random(seed: number): () => number {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function show(decoded: DecodedFrame): void {
  ctx?.drawImage(decoded.frame, 0, 0, view.width, view.height);
}

const quantile = (sorted: number[], q: number) =>
  sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))] ?? 0;
const round = (n: number) => Math.round(n * 10) / 10;
const seconds = (t: Flicks) => flicksToSeconds(t).toFixed(3);

function selectedInput(): { name: string; bytes: number; input: MediaInput } | null {
  const picked = fileInput.files?.[0];
  if (picked) return { name: picked.name, bytes: picked.size, input: picked };
  const option = mediaSelect.selectedOptions[0];
  if (!option?.value) return null;
  return {
    name: option.value,
    bytes: Number(option.dataset["size"]),
    input: { url: `/__test-media/file/${encodeURIComponent(option.value)}` },
  };
}

async function openSelected(): Promise<void> {
  const selected = selectedInput();
  if (!selected) return log("Choose a file first.");
  const t0 = performance.now();
  const info = await client.open(selected.input);
  const ms = round(performance.now() - t0);
  if (!info.ok) return log(`Open failed: ${JSON.stringify(info)}`);
  duration = info.duration;
  // Show the first frame, which also proves the decoder is configured.
  const first = await client.seek(0);
  if (first) {
    show(first);
    first.release();
  }
  const heap = heapMB();
  results.open = { file: selected.name, bytes: selected.bytes, ms, heapMB: heap, info };
  log(
    `Opened ${selected.name} (${(selected.bytes / 1e9).toFixed(2)} GB) in ${ms} ms. ` +
      `${info.width}x${info.height} ${info.codec}, ${seconds(info.duration)} s, ` +
      `rotation ${info.rotation}. Main heap ${heap ?? "n/a"} MB.`,
  );
}

async function seekBench(mode: "exact" | "keyframe", count = 50): Promise<void> {
  if (!duration) return log("Open a file first.");
  const next = random(1);
  const times: number[] = [];
  let wrong = 0;
  let compared = 0;
  let mismatched = 0;
  for (let i = 0; i < count; i++) {
    const target = Math.floor(next() * duration);
    const t0 = performance.now();
    const decoded = await client.seek(target, mode);
    times.push(performance.now() - t0);
    if (!decoded) {
      wrong++;
      continue;
    }
    // An exact seek must return the frame showing at the target time.
    if (mode === "exact" && !(decoded.time <= target && target < decoded.time + decoded.duration)) {
      wrong++;
      log(`  wrong frame: asked ${seconds(target)} s, got ${seconds(decoded.time)} s`);
    }
    show(decoded);
    // Every fifth exact seek: compare pixels with the same frame from a full decode (the play
    // path), which proves that skipping frames during the seek changed nothing.
    if (mode === "exact" && i % 5 === 0) {
      const seekPixels = pixels(decoded.frame);
      decoded.release();
      let reference: Uint8ClampedArray | null = null;
      const playback = client.play(target, target + 1, (f) => {
        reference ??= pixels(f.frame);
        f.release();
      });
      await playback.done;
      const diff = reference ? maxDifference(seekPixels, reference) : 255;
      compared++;
      if (diff > 0) {
        mismatched++;
        log(`  pixel mismatch at ${seconds(target)} s: max difference ${diff}`);
      }
    } else {
      decoded.release();
    }
  }
  times.sort((a, b) => a - b);
  const stats = await client.stats();
  const seek = {
    mode,
    count,
    medianMs: round(quantile(times, 0.5)),
    p90Ms: round(quantile(times, 0.9)),
    maxMs: round(times[times.length - 1] ?? 0),
    wrong,
    compared,
    mismatched,
    skippedFrames: stats.seekFramesSkipped,
    decodedFrames: stats.seekFramesDecoded,
  };
  results.seek = seek;
  log(
    `Seek (${mode}) x${count}: median ${seek.medianMs} ms, p90 ${seek.p90Ms} ms, ` +
      `max ${seek.maxMs} ms, wrong frames ${wrong}, pixel mismatches ${mismatched}/${compared}. ` +
      `Seek work so far: ${stats.seekFramesDecoded} decoded, ${stats.seekFramesSkipped} skipped.`,
  );
}

const probe = new OffscreenCanvas(480, 270);
const probeContext = probe.getContext("2d", { willReadFrequently: true });

/** A downscaled copy of the frame's pixels, for comparison. */
function pixels(frame: VideoFrame): Uint8ClampedArray {
  if (!probeContext) throw new Error("No 2D context");
  probeContext.drawImage(frame, 0, 0, probe.width, probe.height);
  return probeContext.getImageData(0, 0, probe.width, probe.height).data;
}

function maxDifference(a: Uint8ClampedArray, b: Uint8ClampedArray): number {
  let max = 0;
  for (let i = 0; i < a.length; i++) max = Math.max(max, Math.abs((a[i] ?? 0) - (b[i] ?? 0)));
  return max;
}

async function soak(totalSeconds: number): Promise<void> {
  if (!duration) return log("Open a file first.");
  const next = random(2);
  const heapStart = heapMB();
  const end = performance.now() + totalSeconds * 1000;
  let plays = 0;
  let seeks = 0;
  let stops = 0;
  let frames = 0;
  let orderErrors = 0;
  let lastReport = performance.now();
  log(`Soak for ${totalSeconds} s...`);

  while (performance.now() < end) {
    if (next() < 0.7) {
      // Play 0.5-4 s from a random point, holding the newest frame like a renderer would.
      const from = Math.floor(next() * duration);
      const to = Math.min(duration, from + Math.floor((0.5 + next() * 3.5) * 705_600_000));
      let held: DecodedFrame | null = null;
      let last = -1;
      const stopEarly = next() < 0.2;
      const playback = client.play(from, to, (decoded) => {
        if (decoded.time <= last) orderErrors++;
        if (last === -1 && decoded.time > from) orderErrors++;
        last = decoded.time;
        held?.release();
        held = decoded;
        frames++;
        if (stopEarly && frames % 30 === 0) playback.stop();
      });
      await playback.done;
      if (held) {
        show(held);
        (held as DecodedFrame).release();
      }
      plays++;
      if (stopEarly) stops++;
    } else {
      const decoded = await client.seek(
        Math.floor(next() * duration),
        next() < 0.5 ? "exact" : "keyframe",
      );
      decoded?.release();
      seeks++;
    }
    if (performance.now() - lastReport > 30_000) {
      lastReport = performance.now();
      log(
        `  ...${plays} plays, ${seeks} seeks, ${frames} frames, outstanding ${client.outstanding}`,
      );
    }
  }

  // Let any frame still in transit arrive and be returned.
  await new Promise((r) => setTimeout(r, 500));
  const stats = await client.stats();
  results.soak = {
    seconds: totalSeconds,
    plays,
    seeks,
    stops,
    frames,
    orderErrors,
    outstanding: client.outstanding,
    workerInFlight: stats.framesInFlight,
    workerFramesHeld: stats.framesHeld,
    heapStartMB: heapStart,
    heapEndMB: heapMB(),
  };
  log(
    `Soak done: ${plays} plays (${stops} stopped early), ${seeks} seeks, ${frames} frames, ` +
      `${orderErrors} order errors. Leak counter ${client.outstanding}; worker in flight ` +
      `${stats.framesInFlight}, frames held ${stats.framesHeld}. Heap ${heapStart} -> ${heapMB()} MB.`,
  );
}

function guard(task: () => Promise<void>): () => void {
  return () => {
    if (busy) return;
    busy = true;
    task()
      .catch((error: unknown) =>
        log(`Error: ${error instanceof Error ? error.message : String(error)}`),
      )
      .finally(() => (busy = false));
  };
}

$("open").addEventListener("click", guard(openSelected));
$("seek").addEventListener(
  "click",
  guard(() => seekBench("exact")),
);
$("soak").addEventListener(
  "click",
  guard(() => soak(Number(soakSeconds.value) || 300)),
);
Object.assign(window, { bench: { openSelected, seekBench, soak } });

const list = (await (await fetch("/__test-media/list")).json()) as { name: string; size: number }[];
for (const { name, size } of list) {
  const option = new Option(`${name} (${(size / 1e9).toFixed(2)} GB)`, name);
  option.dataset["size"] = String(size);
  mediaSelect.append(option);
}
if (list.length === 0)
  mediaSelect.append(new Option("No test media (run scripts/make-test-media.mjs)", ""));
