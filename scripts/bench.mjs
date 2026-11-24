// M0 engine benchmark. Runs the spike's measurement pages in a real Chromium browser on the
// machine's GPU and writes one JSON file of results next to the spec §6.4 targets.
//
//   node scripts/bench.mjs [--browser msedge|chrome] [--out bench-results/<file>.json] [--quick]
//                          [--fail-on-miss]
//
// Needs the test media (node scripts/make-test-media.mjs) and a GPU. Starts its own dev server.
// --quick shortens the long runs (for trying the runner, not for recorded results).
// --fail-on-miss exits with 1 when a scenario fails or a target is missed (for CI).

import { execSync, spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { chromium } from "playwright-core";

const root = join(import.meta.dirname, "..");
const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const quick = process.argv.includes("--quick");
const failOnMiss = process.argv.includes("--fail-on-miss");
const channel = arg("browser", "msedge");
const stamp = new Date().toISOString().replace(/[:.]/g, "-");
const out = arg("out", join(root, "bench-results", `${stamp}.json`));
const PORT = 5199;
const BASE = `http://localhost:${PORT}`;
const F = 705_600_000;

const log = (...parts) => console.log(new Date().toISOString().slice(11, 19), ...parts);

async function startServer() {
  const server = spawn(`pnpm --filter @renderly/spike exec vite --port ${PORT} --strictPort`, {
    cwd: root,
    shell: true,
    stdio: "ignore",
  });
  // Ready when it answers; its console output is coloured and not worth parsing.
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      if ((await fetch(`${BASE}/index.html`)).ok) return server;
    } catch {
      // not up yet
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error("Dev server did not start");
}

/** Opens `path` in a fresh page and waits until its test hooks are ready. */
async function openPage(context, path) {
  const page = await context.newPage();
  page.on("pageerror", (error) => log("page error:", error.message));
  await page.goto(`${BASE}${path}`);
  await page.waitForFunction(() => window.pageReadyAt !== undefined, null, { timeout: 60_000 });
  return page;
}

const scenarios = {
  /** Cold start: navigation to a usable preview (workers, audio worklet, GL context). */
  async coldStart(context) {
    const page = await openPage(context, "/preview.html");
    const ms = await page.evaluate(() => Math.round(window.pageReadyAt));
    await page.close();
    return { readyMs: ms };
  },

  /** Opening the 6.31 GB 4K file, main heap after, and exact seek latency. */
  async decode(context) {
    const page = await openPage(context, "/decode.html");
    const result = await page.evaluate(async () => {
      document.getElementById("media").value = "a_4k60_h264_120mbps_7min.mp4";
      await window.bench.openSelected();
      await window.bench.seekBench("exact", 50);
      return window.benchResults;
    });
    await page.close();
    return {
      openMs: result.open.ms,
      fileGB: Math.round(result.open.bytes / 1e7) / 100,
      mainHeapMB: result.open.heapMB,
      seekMedianMs: result.seek.medianMs,
      seekP90Ms: result.seek.p90Ms,
      seekWrongFrames: result.seek.wrong,
      seekPixelMismatches: result.seek.mismatched,
    };
  },

  /** Single-layer 4K60 playback on the audio clock. */
  async play4k(context) {
    const page = await openPage(context, "/preview.html");
    const seconds = quick ? 15 : 60;
    const result = await page.evaluate(
      async ({ seconds, F }) => {
        const R = window.previewResults;
        await window.preview.open({ url: "/__test-media/file/a_4k60_h264_120mbps_7min.mp4" });
        R.history.length = 0;
        R.resetMain();
        window.preview.play(100 * F);
        await new Promise((r) => setTimeout(r, seconds * 1000));
        const h = R.history;
        const l = h.at(-1);
        await R.pause();
        const fps = h
          .slice(2)
          .filter((s) => s.playing)
          .map((s) => s.fps);
        return {
          presented: l.presented,
          dropped: l.dropped,
          late: l.late,
          fpsMin: Math.min(...fps),
          outOfSync: l.sync.outOfSync,
          syncChecks: l.sync.checks,
          longTasks: R.main().longTasks,
          gpuPeakMB: l.memory.peakMB,
        };
      },
      { seconds, F },
    );
    await page.close();
    return {
      seconds,
      ...result,
      droppedPercent: Math.round((result.dropped / Math.max(1, result.presented)) * 10000) / 100,
    };
  },

  /** Three 1080p60 layers, and two 4K layers at full and half preview resolution. */
  async layers(context) {
    const page = await openPage(context, "/preview.html");
    const seconds = quick ? 10 : 30;
    const result = await page.evaluate(
      async ({ seconds }) => {
        const R = window.previewResults;
        const run = async (files, specs, scale) => {
          await window.preview.load(
            files.map((f) => ({ url: "/__test-media/file/" + f })),
            R.layerList(specs),
          );
          window.preview.setPreviewScale(scale);
          await new Promise((r) => setTimeout(r, 1000));
          R.history.length = 0;
          window.preview.play(0);
          await new Promise((r) => setTimeout(r, seconds * 1000));
          const h = R.history;
          const l = h.at(-1);
          await R.pause();
          const fps = h
            .slice(2)
            .filter((s) => s.playing)
            .map((s) => s.fps);
          return {
            fpsMin: Math.min(...fps),
            dropped: l.dropped,
            late: l.late,
            gpuPeakMB: l.memory.peakMB,
          };
        };
        const small = { x: 0.25, y: 0.25, scale: 0.45, opacity: 0.85 };
        return {
          threeLayers1080p: await run(
            ["e_1080p60_h264_12min.mp4", "b_1080p60_h264_1min.mp4"],
            [
              { source: 0, inFrame: 6000, frames: 2700 },
              {
                source: 1,
                inFrame: 300,
                frames: 2700,
                transform: { ...small, x: -0.25, y: -0.25, opacity: 1 },
              },
              { source: 0, inFrame: 20000, frames: 2700, transform: small },
            ],
            1,
          ),
          twoLayers4kFull: await run(
            ["a_4k60_h264_120mbps_7min.mp4", "g_4k60_h264_50mbps_3min.mp4"],
            [
              { source: 0, inFrame: 3000, frames: 3000 },
              {
                source: 1,
                inFrame: 1200,
                frames: 3000,
                transform: { x: 0.2, y: 0.2, scale: 0.5, opacity: 0.9 },
              },
            ],
            1,
          ),
          twoLayers4kHalf: await run(
            ["a_4k60_h264_120mbps_7min.mp4", "g_4k60_h264_50mbps_3min.mp4"],
            [
              { source: 0, inFrame: 3000, frames: 3000 },
              {
                source: 1,
                inFrame: 1200,
                frames: 3000,
                transform: { x: 0.2, y: 0.2, scale: 0.5, opacity: 0.9 },
              },
            ],
            0.5,
          ),
        };
      },
      { seconds },
    );
    await page.close();
    return { seconds, ...result };
  },

  /** The seeded cut stress list over H.264 and HEVC 4K60 files. */
  async cuts(context) {
    const page = await openPage(context, "/preview.html");
    const count = quick ? 30 : 100;
    const result = await page.evaluate(
      async ({ count, F }) => {
        const R = window.previewResults;
        const files = [
          "a_4k60_h264_120mbps_7min.mp4",
          "f_4k60_hevc_60mbps_3min.mp4",
          "g_4k60_h264_50mbps_3min.mp4",
        ];
        const cuts = R.stressList([25200, 10800, 10800], count, 1);
        const loaded = await window.preview.load(
          files.map((f) => ({ url: "/__test-media/file/" + f })),
          cuts,
        );
        R.history.length = 0;
        window.preview.play(0);
        await new Promise((r) => setTimeout(r, (loaded.duration / F) * 1000 + 3000));
        const l = [...R.history].reverse().find((s) => s.presented > 0);
        return {
          cuts: count,
          decoderSwitches: l.cuts.passed,
          ...l.cuts,
          dropped: l.dropped,
          late: l.late,
          outOfSync: l.sync.outOfSync,
        };
      },
      { count, F },
    );
    await page.close();
    return result;
  },

  /** A 4-second drag with a seek request on every display frame, after a warm-up drag. */
  async scrub(context) {
    const page = await openPage(context, "/preview.html");
    const result = await page.evaluate(
      async ({ F }) => {
        const R = window.previewResults;
        await window.preview.open({ url: "/__test-media/file/a_4k60_h264_120mbps_7min.mp4" });
        await new Promise((r) => setTimeout(r, 1000));
        await R.scrubTest(100 * F, 10 * F, 1);
        const drag = await R.scrubTest(150 * F, 30 * F, 4);
        await new Promise((r) => setTimeout(r, 1200));
        const s = R.history.at(-1).scrub;
        return {
          updatesPerSecond: Math.round((s.updates / 4) * 10) / 10,
          maxUiFrameMs: drag.maxUiFrameMs,
          restingExact: drag.exact,
        };
      },
      { F },
    );
    await page.close();
    return result;
  },

  /** Export speed and parity. */
  async export(context) {
    const page = await openPage(context, "/preview.html");
    const seconds = quick ? 20 : 120;
    const result = await page.evaluate(
      async ({ seconds, F }) => {
        const R = window.previewResults;
        const sources = [
          { url: "/__test-media/file/e_1080p60_h264_12min.mp4" },
          { url: "/__test-media/file/b_1080p60_h264_1min.mp4" },
        ];
        const settings = {
          width: 1920,
          height: 1080,
          fps: 30,
          videoBitrate: 8e6,
          audioBitrate: 192e3,
        };
        const cuts = [
          { source: 0, in: 100 * F, out: (100 + seconds / 2) * F, start: 0 },
          { source: 1, in: 0, out: (seconds / 2) * F, start: (seconds / 2) * F },
        ];
        const ex = await R.exportTest(sources, cuts, settings, "bench-export.mp4");
        const root = await navigator.storage.getDirectory();
        await root.removeEntry("bench-export.mp4").catch(() => {});
        const layered = R.layerList([
          { source: 0, inFrame: 6000, frames: 1800 },
          {
            source: 1,
            inFrame: 300,
            frames: 1800,
            transform: { x: -0.25, y: -0.25, scale: 0.45, opacity: 1 },
          },
        ]);
        const times = Array.from({ length: 20 }, (_, i) => Math.floor((i * 37) % 899) * (F / 30));
        const parity = await R.parityTest(sources, layered, settings, times);
        return {
          ok: ex.result.ok,
          frames: ex.result.frames,
          exportSeconds: ex.result.seconds,
          parityIdentical: parity.filter((p) => p.sameSize && p.maxDiff === 0).length,
          parityFrames: parity.length,
        };
      },
      { seconds, F },
    );
    await page.close();
    return {
      timelineSeconds: seconds,
      ...result,
      timesRealTime: Math.round((seconds / result.exportSeconds) * 10) / 10,
    };
  },
};

/** Spec §6.4 targets, with what each result is compared against. */
function compare(r) {
  const row = (measure, target, value, pass) => ({ measure, target, value, pass });
  const na = (measure, target, why) => ({ measure, target, value: null, pass: null, note: why });
  return [
    row(
      "Import of a 6 GB file to usable",
      "< 1 s",
      `${r.decode.openMs} ms (${r.decode.fileGB} GB)`,
      r.decode.openMs < 1000,
    ),
    row(
      "Main-thread heap after import",
      "< 50 MB",
      `${r.decode.mainHeapMB} MB`,
      r.decode.mainHeapMB !== null && r.decode.mainHeapMB < 50,
    ),
    row(
      "4K60 single-layer playback",
      "60 fps, < 0.1% dropped",
      `${r.play4k.fpsMin} fps min, ${r.play4k.droppedPercent}% dropped`,
      r.play4k.fpsMin >= 59 && r.play4k.droppedPercent < 0.1,
    ),
    row(
      "1080p, 3 video layers",
      "60 fps",
      `${r.layers.threeLayers1080p.fpsMin} fps min`,
      r.layers.threeLayers1080p.fpsMin >= 59,
    ),
    row(
      "Exact seek, 4K long-GOP",
      "median < 120 ms",
      `${r.decode.seekMedianMs} ms`,
      r.decode.seekMedianMs < 120,
    ),
    row(
      "Scrub picture update while dragging",
      "≥ 30 per second",
      `${r.scrub.updatesPerSecond}/s, worst UI frame ${r.scrub.maxUiFrameMs} ms`,
      r.scrub.updatesPerSecond >= 30,
    ),
    na("Timeline redraw, 500 clips", "< 2 ms", "No timeline UI until M1"),
    na("Non-AI effect stack per frame, 720p", "< 4 ms", "No effects until M2"),
    row(
      "Export, 1080p30, no AI effects",
      "faster than real time",
      `${r.export.timesRealTime}× real time`,
      r.export.timesRealTime > 1,
    ),
    row(
      "Cold start to interactive",
      "< 2 s on a warm cache",
      `${r.coldStart.readyMs} ms (spike page)`,
      r.coldStart.readyMs < 2000,
    ),
  ];
}

async function main() {
  mkdirSync(dirname(out), { recursive: true });
  log("starting dev server");
  const server = await startServer();
  const browser = await chromium.launch({
    channel,
    headless: true,
    args: ["--ignore-gpu-blocklist", "--autoplay-policy=no-user-gesture-required"],
  });
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  try {
    const probe = await openPage(context, "/preview.html").catch(() => null);
    const environment = probe
      ? await probe.evaluate(() => {
          const gl = new OffscreenCanvas(1, 1).getContext("webgl2");
          const info = gl?.getExtension("WEBGL_debug_renderer_info");
          return {
            userAgent: navigator.userAgent,
            gpu: info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : null,
            crossOriginIsolated,
          };
        })
      : null;
    await probe?.close();
    log("gpu:", environment?.gpu);
    if (environment?.gpu && /swiftshader|software/i.test(environment.gpu)) {
      throw new Error(
        `No hardware GPU in the browser (${environment.gpu}); results would be meaningless`,
      );
    }

    const results = {};
    for (const [name, scenario] of Object.entries(scenarios)) {
      log("running", name);
      const started = Date.now();
      try {
        results[name] = await scenario(context);
      } catch (error) {
        results[name] = { error: String(error) };
      }
      log(
        name,
        "done in",
        Math.round((Date.now() - started) / 1000),
        "s:",
        JSON.stringify(results[name]),
      );
    }

    let commit = null;
    try {
      commit = execSync("git rev-parse --short HEAD", { cwd: root }).toString().trim();
    } catch {
      // not a git checkout
    }
    let targets = null;
    try {
      targets = compare(results);
    } catch (error) {
      targets = { error: `A scenario failed, so targets could not be compared: ${String(error)}` };
    }
    const report = {
      date: new Date().toISOString(),
      commit,
      browser: channel,
      quick,
      environment,
      results,
      targets,
    };
    writeFileSync(out, JSON.stringify(report, null, 2));
    log("wrote", out);
    if (Array.isArray(targets)) {
      for (const t of targets)
        log(
          t.pass === null ? "  n/a " : t.pass ? "  PASS" : "  FAIL",
          t.measure,
          "—",
          t.value ?? t.note,
        );
    }
    const missed = !Array.isArray(targets) || targets.some((t) => t.pass === false);
    if (failOnMiss && missed) process.exitCode = 1;
  } finally {
    await browser.close();
    server.kill();
    if (process.platform === "win32") {
      try {
        execSync(`taskkill /pid ${server.pid} /T /F`, { stdio: "ignore" });
      } catch {
        // already gone
      }
    }
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
