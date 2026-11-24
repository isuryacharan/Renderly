// S0.3 preview page. Measurements are kept on `window.previewResults` for a test driver.

import "@renderly/ui/fonts.css";
import "@renderly/ui/styles.css";
import "./spike.css";
import { createPreview, type PreviewScale, type RenderStats } from "@renderly/engine";
import { flicksToSeconds, type MediaInput } from "@renderly/media";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const mediaSelect = $<HTMLSelectElement>("media");
const fileInput = $<HTMLInputElement>("file");
const statsEl = $<HTMLPreElement>("stats");
const logEl = $<HTMLPreElement>("log");

const preview = createPreview($<HTMLCanvasElement>("view"));
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

const results = {
  history,
  errors,
  main: () => ({ longTasks, maxTimerLagMs: Math.round(maxTimerLagMs) }),
  resetMain: () => {
    longTasks = 0;
    maxTimerLagMs = 0;
  },
};
Object.assign(window, { previewResults: results, preview });

function log(line: string): void {
  logEl.textContent += `${line}\n`;
}

preview.onError((message) => {
  errors.push(message);
  log(`Error: ${message}`);
});

preview.onStats((stats) => {
  history.push(stats);
  if (history.length > 600) history.shift();
  statsEl.textContent =
    `${stats.playing ? "playing" : "paused"} at ${flicksToSeconds(stats.playhead).toFixed(2)} s\n` +
    `fps ${stats.fps}  presented ${stats.presented}  dropped ${stats.dropped}  late ${stats.late}\n` +
    `worker tick mean ${stats.tickMeanMs} ms, max ${stats.tickMaxMs} ms\n` +
    `working ${stats.working.width}x${stats.working.height}  frames on loan ${stats.framesOutstanding}\n` +
    `context ${stats.contextLost ? "LOST" : "ok"}, restored ${stats.contextRestores} times\n` +
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
  const result = await preview.open(selected.input);
  log(
    result.ok
      ? `Opened ${selected.name}: ${result.width}x${result.height} ${result.codec}, rotation ${result.rotation}`
      : `Cannot open ${selected.name}: ${JSON.stringify(result)}`,
  );
}

let playhead = 0;
preview.onStats((stats) => (playhead = stats.playhead));

$("open").addEventListener("click", () => void open());
$("play").addEventListener("click", () => preview.play(playhead));
$("pause").addEventListener("click", () => preview.pause());
$("start").addEventListener("click", () => {
  playhead = 0;
  void preview.seek(0);
});
$<HTMLSelectElement>("scale").addEventListener("change", (event) => {
  preview.setPreviewScale(Number((event.target as HTMLSelectElement).value) as PreviewScale);
});
$("lose").addEventListener("click", () => preview.loseContext(1000));

const list = (await (await fetch("/__test-media/list")).json()) as { name: string }[];
for (const { name } of list) mediaSelect.append(new Option(name, name));
