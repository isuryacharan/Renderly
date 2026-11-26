// Golden frames: each case in apps/spike/src/golden-page.ts is rendered by the compositor on this
// machine's GPU (software rendering on CI) and compared with the CPU reference. A case passes
// when no more than OUTLIER_SHARE of its pixels differ by more than TOLERANCE 8-bit steps in any
// channel. On failure the GPU picture, the reference and a difference map are written to
// test-results/golden/.
//
//   pnpm test:golden                    installed Edge
//   pnpm test:golden --browser chrome   installed Chrome (CI)

import { execSync, spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright-core";

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const channel = arg("browser", "msedge");
const root = join(import.meta.dirname, "..");
const PORT = 5184;
const BASE = `http://localhost:${PORT}`;
/** Allowed difference per channel, in 8-bit steps (half-float storage and GPU rounding). */
const TOLERANCE = 3;
/** Pixels allowed past it: those whose sample lands exactly on a layer's edge can go either way. */
const OUTLIER_SHARE = 0.002;

const log = (...parts) => console.log(new Date().toISOString().slice(11, 19), ...parts);

async function startServer() {
  // A server left on the port from an earlier, interrupted run would answer instead of ours (and
  // ours would fail to start without a word): refuse rather than test the wrong server.
  const busy = async () =>
    fetch(`${BASE}/`).then(
      () => true,
      () => false,
    );
  for (let i = 0; i < 25 && (await busy()); i++) await new Promise((r) => setTimeout(r, 200));
  if (await busy())
    throw new Error(
      `Port ${PORT} is already in use, probably by a dev server from an earlier run. Stop it and try again.`,
    );
  const server = spawn(`pnpm --filter @renderly/spike exec vite --port ${PORT} --strictPort`, {
    detached: process.platform !== "win32",
    cwd: root,
    shell: true,
    stdio: "ignore",
  });
  // Stop the server if this run is interrupted (Ctrl+C or a kill), so it does not outlive us.
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"])
    process.once(signal, () => void stop(server).then(() => process.exit(130)));
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    if (await busy()) return server;
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error("Dev server did not start");
}

/**
 * Stops the dev server and everything it started, and waits until its port is free, so the next
 * run can start its own server on it.
 */
async function stop(server) {
  if (process.platform === "win32") {
    try {
      execSync(`taskkill /pid ${server.pid} /T /F`, { stdio: "ignore" });
    } catch {
      // already gone
    }
  } else {
    try {
      process.kill(-server.pid, "SIGTERM");
    } catch {
      server.kill();
    }
  }
  const deadline = Date.now() + 10_000;
  while (
    Date.now() < deadline &&
    (await fetch(`${BASE}/`).then(
      () => true,
      () => false,
    ))
  )
    await new Promise((resolve) => setTimeout(resolve, 200));
}

const saveImage = (path, dataUrl) =>
  writeFileSync(path, Buffer.from(dataUrl.slice(dataUrl.indexOf(",") + 1), "base64"));

async function main() {
  log(`starting dev server; browser: ${channel}`);
  const server = await startServer();
  const browser = await chromium.launch({ channel, headless: true });
  let failed = 0;
  try {
    const page = await browser.newPage();
    const pageErrors = [];
    page.on("pageerror", (error) => pageErrors.push(error.message));
    await page.goto(`${BASE}/golden.html`);
    await page.waitForFunction(() => window.pageReadyAt !== undefined, null, { timeout: 60_000 });
    log(
      "gpu:",
      await page.evaluate(() => {
        const gl = document.createElement("canvas").getContext("webgl2");
        const info = gl?.getExtension("WEBGL_debug_renderer_info");
        return info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : "unknown";
      }),
    );
    const cases = await page.evaluate(() => window.golden.cases());
    for (const name of cases) {
      const result = await page.evaluate(([n, t]) => window.golden.run(n, t), [name, TOLERANCE]);
      const pass = result.outliers <= OUTLIER_SHARE;
      const detail = `max ${result.max}, mean ${result.mean.toFixed(2)}, outliers ${(result.outliers * 100).toFixed(3)}%`;
      log(`${pass ? "PASS" : "FAIL"} ${name} — ${detail}`);
      if (!pass) {
        failed++;
        const images = await page.evaluate(
          ([n, t]) => window.golden.run(n, t, true),
          [name, TOLERANCE],
        );
        const dir = join(root, "test-results", "golden");
        mkdirSync(dir, { recursive: true });
        const file = name.replace(/[^a-z0-9]+/gi, "-").toLowerCase();
        for (const kind of ["gpu", "reference", "difference"])
          saveImage(join(dir, `${file}.${kind}.png`), images[kind]);
        log(`  wrote the pictures to test-results/golden/${file}.*.png`);
      }
    }
    if (pageErrors.length) {
      failed++;
      log(`FAIL uncaught page errors: ${pageErrors.join("; ")}`);
    }
  } finally {
    await browser.close();
    await stop(server);
  }
  log(failed === 0 ? "all golden frames match" : `${failed} golden frame check(s) failed`);
  process.exit(failed === 0 ? 0 : 1);
}

await main();
