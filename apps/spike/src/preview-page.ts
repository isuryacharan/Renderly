// S0.3 / S0.4 preview page. Measurements are kept on `window.previewResults` for a test driver.

import "@renderly/ui/fonts.css";
import "@renderly/ui/styles.css";
import "./spike.css";
import { AudioRing, OUTPUT_SAMPLE_RATE, serveAudioDecoder } from "@renderly/audio";
import { createPreview, type Cut, type PreviewScale, type RenderStats } from "@renderly/engine";
import {
  FLICKS_PER_SECOND,
  flicksToSeconds,
  type Endpoint,
  type MediaInput,
} from "@renderly/media";
import { beepOnsets } from "./beeps";
import { layerList, shortClipList, stressList } from "./stress";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const mediaSelect = $<HTMLSelectElement>("media");
const fileInput = $<HTMLInputElement>("file");
const statsEl = $<HTMLPreElement>("stats");
const logEl = $<HTMLPreElement>("log");

const preview = await createPreview($<HTMLCanvasElement>("view"));
const history: RenderStats[] = [];
const errors: string[] = [];

// Main-thread load while the workers play: long tasks (> 50 ms) and timer lag.
let longTasks = 0;
new PerformanceObserver((list) => (longTasks += list.getEntries().length)).observe({
  type: "longtask",
});
let maxTimerLagMs = 0;
let expected = performance.now() + 100;
setInterval(() => {
  const now = performance.now();
  maxTimerLagMs = Math.max(maxTimerLagMs, now - expected);
  expected = now + 100;
}, 100);

function log(line: string): void {
  logEl.textContent += `${line}\n`;
}

preview.onError((message) => {
  errors.push(message);
  log(`Error: ${message}`);
});

let playhead = 0;
preview.onStats((stats) => {
  history.push(stats);
  if (history.length > 900) history.shift();
  if (stats.playing) playhead = stats.playhead;
  const audio = preview.audio.diagnostics();
  statsEl.textContent =
    `${stats.playing ? "playing" : "paused"} at ${flicksToSeconds(stats.playhead).toFixed(2)} s` +
    ` (${stats.audioClock ? "audio clock" : "display clock"})\n` +
    `fps ${stats.fps}  presented ${stats.presented}  dropped ${stats.dropped}  late ${stats.late}\n` +
    `sync: ${stats.sync.outOfSync} of ${stats.sync.checks} refreshes out of sync, offset ` +
    `${stats.sync.minMs}..${stats.sync.maxMs} ms\n` +
    `audio: ${audio.underruns} underruns, largest step ${audio.maxStep.toFixed(4)} steady, ` +
    `${audio.maxStepTransition.toFixed(4)} at starts and stops\n` +
    `worker tick mean ${stats.tickMeanMs} ms, max ${stats.tickMaxMs} ms; start ${stats.startLatencyMs} ms\n` +
    `working ${stats.working.width}x${stats.working.height}  frames on loan ${stats.framesOutstanding}\n` +
    `context ${stats.contextLost ? "LOST" : "ok"}, restored ${stats.contextRestores} times\n` +
    `cuts: ${stats.cuts.passed} passed, ${stats.cuts.held} held (max ${stats.cuts.maxHoldMs} ms), ` +
    `${stats.cuts.inexact} inexact, ${stats.cuts.droppedAtCuts} frames dropped at cuts
` +
    `decoders: ${stats.decoders.busy} of ${stats.decoders.pool} busy, ${stats.decoders.opens} opens
` +
    `layers: ${stats.layers.map((l) => `${l.layer}: ${l.fps} fps${l.decoding ? "" : ` (keyframes ${l.keyframes})`}`).join(", ")}
` +
    `GPU memory: ${stats.memory.usedMB} MB, peak ${stats.memory.peakMB} of ${stats.memory.ceilingMB} MB` +
    `${stats.memory.refused ? `, ${stats.memory.refused} targets refused` : ""}
` +
    `main thread: ${longTasks} long tasks, max timer lag ${Math.round(maxTimerLagMs)} ms`;
});

function selectedInput(): { name: string; input: MediaInput } | null {
  const picked = fileInput.files?.[0];
  if (picked) return { name: picked.name, input: picked };
  const name = mediaSelect.value;
  return name ? { name, input: { url: `/__test-media/file/${encodeURIComponent(name)}` } } : null;
}

async function open(): Promise<void> {
  const selected = selectedInput();
  if (!selected) return log("Choose a file first.");
  const { video, audio } = await preview.open(selected.input);
  playhead = 0;
  log(
    video.ok
      ? `Opened ${selected.name}: ${video.width}x${video.height} ${video.codec}, rotation ` +
          `${video.rotation}; audio ${audio.ok ? `${audio.codec} ${audio.channels} ch` : audio.reason}`
      : `Cannot open ${selected.name}: ${JSON.stringify(video)}`,
  );
}

async function pause(): Promise<number> {
  playhead = await preview.pause();
  return playhead;
}

/**
 * Decodes `seconds` of an edit list's audio from timeline time `from`, through the same decoder
 * code the audio worker runs, and returns where the beeps land, in samples from `from`.
 * One source and no cuts means the whole file.
 */
async function decodedBeeps(
  sources: MediaInput[],
  cuts: Cut[] | null,
  from: number,
  seconds: number,
): Promise<number[]> {
  const channel = new MessageChannel();
  serveAudioDecoder(channel.port1 as unknown as Endpoint);
  const port = channel.port2;
  const loaded = new Promise<{ type: string; results?: { ok: boolean }[] }>((resolve) => {
    port.onmessage = (event: MessageEvent<{ type: string; results?: { ok: boolean }[] }>) =>
      resolve(event.data);
  });
  port.postMessage({ type: "load", id: 1, sources });
  const reply = await loaded;
  if (!reply.results?.some((result) => result.ok)) throw new Error("No usable audio");
  const frames = Math.round(seconds * OUTPUT_SAMPLE_RATE);
  const ring = AudioRing.create(1, frames + 1);
  const list = cuts ?? [{ source: 0, in: 0, out: 24 * 3600 * FLICKS_PER_SECOND, start: 0 }];
  port.postMessage({
    type: "fill",
    session: 1,
    ring: ring.buffer,
    channels: 1,
    capacity: frames + 1,
    from,
    cuts: list,
  });
  while (ring.readable < frames) await new Promise((r) => setTimeout(r, 20));
  port.postMessage({ type: "stop" });
  const samples = new Float32Array(frames);
  ring.read([samples], 0, frames);
  port.close();
  return beepOnsets(samples, OUTPUT_SAMPLE_RATE);
}

/**
 * Drags the playhead for `seconds`, one scrub request per animation frame, sweeping back and forth
 * over `span` flicks from `from`. Measures main-thread frame times, then ends the drag and checks
 * that the resting picture is the exact frame for the last position.
 */
async function scrubTest(from: number, span: number, seconds = 4) {
  const frames: number[] = [];
  let last = performance.now();
  const startedAt = last;
  let target = from;
  await new Promise<void>((resolve) => {
    const step = (now: number) => {
      frames.push(now - last);
      last = now;
      const phase = ((now - startedAt) / 1000 / seconds) * 2;
      const tri = phase % 2 < 1 ? phase % 1 : 1 - (phase % 1);
      target = Math.round(from + tri * span);
      preview.scrub(target);
      if (now - startedAt < seconds * 1000) requestAnimationFrame(step);
      else resolve();
    };
    requestAnimationFrame(step);
  });
  const resting = await preview.scrubEnd();
  const frame = FLICKS_PER_SECOND / 60;
  return {
    requests: frames.length,
    maxUiFrameMs: Math.round(Math.max(...frames.slice(1)) * 10) / 10,
    uiFramesOver20ms: frames.slice(1).filter((ms) => ms > 20).length,
    /** When the slow frames happened, in ms from the start of the drag. */
    slowFramesAtMs: frames
      .map((ms, i) => ({ ms, at: frames.slice(0, i + 1).reduce((sum, x) => sum + x, 0) }))
      .slice(1)
      .filter((frame) => frame.ms > 20)
      .map((frame) => Math.round(frame.at)),
    target,
    resting,
    exact: resting <= target && target < resting + frame,
  };
}

const results = {
  history,
  errors,
  main: () => ({ longTasks, maxTimerLagMs: Math.round(maxTimerLagMs) }),
  resetMain: () => {
    longTasks = 0;
    maxTimerLagMs = 0;
  },
  pause,
  decodedBeeps,
  stressList,
  shortClipList,
  layerList,
  scrubTest,
  FLICKS_PER_SECOND,
};
Object.assign(window, { previewResults: results, preview });

$("open").addEventListener("click", () => void open());
$("play").addEventListener("click", () => preview.play(playhead));
$("pause").addEventListener("click", () => void pause());
$("start").addEventListener("click", () => {
  playhead = 0;
  void preview.seek(0);
});
$<HTMLSelectElement>("scale").addEventListener("change", (event) => {
  preview.setPreviewScale(Number((event.target as HTMLSelectElement).value) as PreviewScale);
});
$("lose").addEventListener("click", () => preview.loseContext(1000));

// The test-media list exists only on the dev server; a production build uses the file picker.
const list = (await fetch("/__test-media/list")
  .then((response) => response.json())
  .catch(() => [])) as { name: string }[];
for (const { name } of list) mediaSelect.append(new Option(name, name));
