// Everything CI checks, run locally before a push, in CI's order, under conditions that catch what
// a fast, warm machine hides:
// - a cold Vite cache (CI always starts cold; dependency discovery at run time reloads pages);
// - the timeline and viewer suite again with every decoder seek slowed down (races that only
//   open when exact seeks are slow, as with software decoding on CI);
// - optionally several runs, so a check that fails one time in three is seen before merging.
//
//   pnpm verify                     all checks once, browser suites in Chrome (as CI)
//   pnpm verify --repeat 3          browser suites three times (the first from a cold cache)
//   pnpm verify --browser msedge    browser suites in Edge
//   pnpm verify --slow-seek 1500    slow seeks by 1500 ms (default 800; 0 skips that pass)
//   pnpm verify --from timeline     start at the first step whose name contains "timeline"
//
// Exits 1 if anything failed; the summary lists every step with its time.

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dirname, "..");
const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const repeat = Math.max(1, Number(arg("repeat", "1")));
const browser = arg("browser", "chrome");
// 800 ms: slower than CI's software decoding, which 300 ms was not (a fixed 500 ms wait in a check
// passed locally at 300 ms and failed on CI).
const slowSeek = Number(arg("slow-seek", "800"));
const from = arg("from", null);

const log = (...parts) => console.log(new Date().toISOString().slice(11, 19), ...parts);

/** Vite's dependency caches: removed so the browser suites start the way CI does. */
const VITE_CACHES = ["apps/editor/node_modules/.vite", "apps/spike/node_modules/.vite"];

function coldCache() {
  for (const dir of VITE_CACHES) rmSync(join(root, dir), { recursive: true, force: true });
}

function run(command) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(command, { cwd: root, shell: true, stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    const keep = (chunk) => {
      output += chunk;
      process.stdout.write(chunk);
    };
    child.stdout.on("data", keep);
    child.stderr.on("data", keep);
    child.on("close", (code) => resolve({ ok: code === 0, ms: Date.now() - started, output }));
  });
}

const steps = [
  { name: "typecheck", command: "pnpm typecheck" },
  { name: "lint", command: "pnpm lint" },
  { name: "format", command: "pnpm format:check" },
  { name: "tokens", command: "pnpm check:tokens" },
  { name: "icons", command: "pnpm check:icons" },
  { name: "unit tests", command: "pnpm test" },
];
for (let i = 1; i <= repeat; i++) {
  const pass = repeat > 1 ? ` (run ${i} of ${repeat})` : "";
  const cold = i === 1;
  steps.push(
    { name: `storage${pass}`, command: `pnpm test:storage --browser ${browser}`, cold },
    { name: `editor${pass}`, command: `pnpm test:editor --browser ${browser}` },
    { name: `media${pass}`, command: `pnpm test:media --browser ${browser}` },
    { name: `timeline${pass}`, command: `pnpm test:timeline --browser ${browser}` },
  );
  if (slowSeek > 0)
    steps.push({
      name: `timeline, seeks +${slowSeek} ms${pass}`,
      command: `pnpm test:timeline --browser ${browser} --slow-seek ${slowSeek}`,
    });
}
steps.push({ name: "build", command: "pnpm build" });

const start = from ? steps.findIndex((step) => step.name.includes(from)) : 0;
if (start < 0) throw new Error(`No step matches "${from}"`);

// The browser suites start their own dev servers on these ports; one left over from an earlier,
// interrupted run would make them fail. Say so up front.
const TEST_PORTS = [5181, 5182, 5183, 5198];
const busy = [];
for (const port of TEST_PORTS)
  if (
    await fetch(`http://localhost:${port}/`).then(
      () => true,
      () => false,
    )
  )
    busy.push(port);
if (busy.length > 0 && steps.slice(start).some((step) => step.command.includes("test:"))) {
  console.error(
    `Ports ${busy.join(", ")} are in use, probably by dev servers left from an earlier run. Stop them first.`,
  );
  process.exit(1);
}

const results = [];
for (const step of steps.slice(start)) {
  log(`▶ ${step.name}`);
  // The first browser pass starts from a cold cache, the way CI does; so does a --from start.
  if (step.cold || (from && step === steps[start] && step.command.includes("test:"))) coldCache();
  const result = await run(step.command);
  results.push({ ...step, ...result });
  log(`${result.ok ? "✔" : "✘"} ${step.name} (${(result.ms / 1000).toFixed(1)} s)`);
}

console.log("\nSummary");
for (const r of results)
  console.log(`  ${r.ok ? "PASS" : "FAIL"}  ${r.name.padEnd(36)} ${(r.ms / 1000).toFixed(1)} s`);
const failed = results.filter((r) => !r.ok);
console.log(
  failed.length === 0
    ? `\nAll ${results.length} steps passed. Safe to push.`
    : `\n${failed.length} of ${results.length} steps failed: ${failed.map((r) => r.name).join(", ")}. Do not push.`,
);
process.exit(failed.length === 0 ? 0 : 1);
