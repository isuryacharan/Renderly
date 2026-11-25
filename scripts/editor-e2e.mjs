// S1.5 app shell checks in a real browser: the home screen's project actions and the editor's
// panels, driven by the keyboard only (Tab, arrows, Enter, Escape), plus the unsupported-browser
// message.
//
//   pnpm test:editor                  installed Edge
//   pnpm test:editor --browser chrome installed Chrome (CI)

import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const channel = arg("browser", "msedge");
const only = arg("only", null);
const PORT = 5181;
const BASE = `http://localhost:${PORT}`;

const log = (...parts) => console.log(new Date().toISOString().slice(11, 19), ...parts);

async function startServer() {
  const server = spawn(`pnpm --filter @renderly/editor exec vite --port ${PORT} --strictPort`, {
    cwd: new URL("..", import.meta.url),
    shell: true,
    stdio: "ignore",
  });
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      if ((await fetch(`${BASE}/`)).ok) return server;
    } catch {
      // not up yet
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error("Dev server did not start");
}

function stop(server) {
  if (process.platform === "win32") spawn("taskkill", ["/pid", String(server.pid), "/T", "/F"]);
  else server.kill();
}

class Failure extends Error {}
function check(condition, message) {
  if (!condition) throw new Failure(message);
}

async function openApp(context, hash = "#/") {
  const page = await context.newPage();
  page.on("pageerror", (error) => log("page error:", error.message));
  await page.goto(`${BASE}/${hash}`);
  await page.waitForFunction(() => window.appReadyAt !== undefined);
  return page;
}

/** The accessible name of the focused element: aria-label, else its text. */
const focusedName = (page) =>
  page.evaluate(() => {
    const el = document.activeElement;
    if (!el || el === document.body) return "";
    return (el.getAttribute("aria-label") ?? el.textContent ?? "").trim().replace(/\s+/g, " ");
  });

/** Presses Tab until the focused control's name starts with `name`, as a keyboard user would. */
async function tabTo(page, name, limit = 60) {
  for (let i = 0; i < limit; i++) {
    await page.keyboard.press("Tab");
    if ((await focusedName(page)).startsWith(name)) return;
  }
  throw new Failure(`could not reach "${name}" with Tab`);
}

async function activate(page, name) {
  await tabTo(page, name);
  await page.keyboard.press("Enter");
}

const until = (page, fn, arg) => page.waitForFunction(fn, arg, { timeout: 10_000, polling: 50 });
const cardsAre = (page, names) =>
  until(
    page,
    (expected) =>
      JSON.stringify(
        [...document.querySelectorAll(".r-card__name")].map((el) => el.textContent).sort(),
      ) === JSON.stringify([...expected].sort()),
    names,
  );

/** Every visible control that Tab should reach, by a selector-like description. */
const tabbables = (page) =>
  page.evaluate(() =>
    [
      ...document.querySelectorAll(
        'button, input, select, textarea, a[href], [tabindex]:not([tabindex="-1"])',
      ),
    ]
      .filter((el) => !el.disabled && el.tabIndex >= 0 && el.checkVisibility())
      .map((el) =>
        (el.getAttribute("aria-label") ?? el.textContent ?? el.id).trim().replace(/\s+/g, " "),
      ),
  );

/** Tabs through the page twice over and returns the controls never reached. */
async function unreached(page) {
  const all = await tabbables(page);
  const seen = new Set();
  for (let i = 0; i < all.length * 2 + 4; i++) {
    await page.keyboard.press("Tab");
    seen.add(await focusedName(page));
  }
  return all.filter((name) => !seen.has(name));
}

const scenarios = {
  async "home: create, rename, duplicate, delete, restore and purge by keyboard"({ context }) {
    const page = await openApp(context);
    await activate(page, "New project");
    await page.keyboard.type("Keyboard project"); // the name is selected, so this replaces it
    await page.keyboard.press("Enter");
    await until(page, () => location.hash.startsWith("#/project/"));
    await until(page, () => document.title.startsWith("Keyboard project"));
    await activate(page, "Home");
    await cardsAre(page, ["Keyboard project"]);

    await activate(page, "More actions for Keyboard project");
    await page.keyboard.press("ArrowDown"); // Open → Rename…
    check((await focusedName(page)) === "Rename…", "the menu moves with the arrows");
    await page.keyboard.press("Enter");
    await page.keyboard.type("Renamed by keys");
    await page.keyboard.press("Enter");
    await cardsAre(page, ["Renamed by keys"]);

    await activate(page, "More actions for Renamed by keys");
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("ArrowDown");
    check((await focusedName(page)) === "Duplicate", "Duplicate is third");
    await page.keyboard.press("Enter");
    await cardsAre(page, ["Renamed by keys", "Renamed by keys copy"]);

    await activate(page, "More actions for Renamed by keys copy");
    await page.keyboard.press("End");
    check((await focusedName(page)) === "Delete", "End goes to Delete");
    await page.keyboard.press("Enter");
    await cardsAre(page, ["Renamed by keys"]);

    await activate(page, "Recycle bin (1)");
    await activate(page, "Restore");
    await cardsAre(page, ["Renamed by keys", "Renamed by keys copy"]);
    await page.keyboard.press("Escape");

    await activate(page, "More actions for Renamed by keys copy");
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    await cardsAre(page, ["Renamed by keys"]);
    await activate(page, "Recycle bin (1)");
    await activate(page, "Delete for good");
    await activate(page, "Delete");
    await until(page, () => document.body.textContent.includes("The recycle bin is empty."));
    await page.keyboard.press("Escape");

    const folders = await page.evaluate(async () => {
      const root = await (await navigator.storage.getDirectory()).getDirectoryHandle("projects");
      const names = [];
      for await (const handle of root.values()) names.push(handle.name);
      const bin = await root.getDirectoryHandle(".recycle");
      for await (const handle of bin.values()) names.push(`.recycle/${handle.name}`);
      return names.sort();
    });
    check(
      folders.length === 2 &&
        folders[0] === ".recycle" &&
        folders[1].startsWith("keyboard-project-"),
      `folders on storage: ${folders.join(", ")}`,
    );
    return `storage holds ${folders.join(", ")}`;
  },

  async "editor: panel sizes survive a reload"({ context }) {
    const page = await openApp(context);
    await activate(page, "New project");
    await page.keyboard.press("Enter");
    await until(page, () => location.hash.startsWith("#/project/"));
    const sizes = () =>
      page.evaluate(() => {
        const width = (label) =>
          Math.round(
            document.querySelector(`section[aria-label="${label}"]`).getBoundingClientRect().width,
          );
        const height = (label) =>
          Math.round(
            document.querySelector(`section[aria-label="${label}"]`).getBoundingClientRect().height,
          );
        return {
          library: width("Library"),
          inspector: width("Inspector"),
          timeline: height("Timeline"),
        };
      });
    const before = await sizes();
    await tabTo(page, "Resize the library");
    for (let i = 0; i < 4; i++) await page.keyboard.press("ArrowRight");
    await tabTo(page, "Resize the inspector");
    await page.keyboard.press("Shift+ArrowLeft"); // inverted: left grows the inspector
    await tabTo(page, "Resize the timeline");
    await page.keyboard.press("ArrowUp"); // inverted: up grows the timeline
    await page.keyboard.press("ArrowUp");
    const after = await sizes();
    check(after.library === before.library + 64, `library ${before.library} → ${after.library}`);
    check(
      after.inspector === before.inspector + 64,
      `inspector ${before.inspector} → ${after.inspector}`,
    );
    check(
      after.timeline === before.timeline + 32,
      `timeline ${before.timeline} → ${after.timeline}`,
    );
    await page.reload();
    await page.waitForFunction(() => window.appReadyAt !== undefined);
    await until(page, () => document.querySelector('section[aria-label="Library"]') !== null);
    const reloaded = await sizes();
    check(
      JSON.stringify(reloaded) === JSON.stringify(after),
      `after reload ${JSON.stringify(reloaded)}, expected ${JSON.stringify(after)}`,
    );
    return `library ${before.library}→${after.library}, inspector ${before.inspector}→${after.inspector}, timeline ${before.timeline}→${after.timeline} px, kept after reload`;
  },

  async "every control is reachable by keyboard"({ context }) {
    const page = await openApp(context);
    await activate(page, "New project");
    await page.keyboard.press("Enter");
    await until(page, () => location.hash.startsWith("#/project/"));
    await until(page, () => document.querySelector("#project-name") !== null);
    const editor = await unreached(page);
    check(editor.length === 0, `not reachable in the editor: ${editor.join(", ")}`);
    const editorCount = (await tabbables(page)).length;
    await activate(page, "Home");
    await until(page, () => document.querySelector(".r-card") !== null);
    const home = await unreached(page);
    check(home.length === 0, `not reachable on home: ${home.join(", ")}`);
    const homeCount = (await tabbables(page)).length;
    // An open dialog makes the page behind it inert: Tab moves between the dialog's controls and
    // the browser's own interface (focus on <body>), never to the page behind.
    await activate(page, "New project");
    const outside = [];
    for (let i = 0; i < 8; i++) {
      await page.keyboard.press("Tab");
      const where = await page.evaluate(() => {
        const el = document.activeElement;
        return !el || el === document.body || el.closest("dialog")
          ? null
          : el.outerHTML.slice(0, 60);
      });
      if (where) outside.push(where);
    }
    check(outside.length === 0, `focus left the dialog: ${outside.join(", ")}`);
    await page.keyboard.press("Escape");
    return `${homeCount} controls on home, ${editorCount} in the editor, all reached with Tab`;
  },

  async "an unsupported browser gets a clear message"({ context }) {
    // Without cross-origin isolation there is no shared memory: the probe must say so.
    await context.route("**/*", async (route) => {
      const response = await route.fetch();
      const headers = { ...response.headers() };
      delete headers["cross-origin-opener-policy"];
      delete headers["cross-origin-embedder-policy"];
      await route.fulfill({ response, headers });
    });
    const page = await openApp(context);
    const text = await page.evaluate(() => document.body.innerText);
    await context.unrouteAll({ behavior: "ignoreErrors" }); // no request left mid-route at close
    check(text.includes("Renderly cannot run in this browser"), "the message is shown");
    check(text.includes("Shared memory"), "it names what is missing");
    return text.split("\n").find((line) => line.startsWith("Shared memory"));
  },
};

async function main() {
  log(`starting dev server; browser: ${channel}`);
  const server = await startServer();
  const browser = await chromium.launch({ channel, headless: true });
  let failed = 0;
  try {
    for (const [name, run] of Object.entries(scenarios)) {
      if (only && !name.includes(only)) continue;
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const started = Date.now();
      try {
        const detail = await run({ context });
        log(`PASS ${name} (${Date.now() - started} ms)${detail ? ` — ${detail}` : ""}`);
      } catch (error) {
        failed++;
        log(`FAIL ${name}: ${error instanceof Error ? error.message : String(error)}`);
      } finally {
        await context.close();
      }
    }
  } finally {
    await browser.close();
    stop(server);
  }
  log(failed === 0 ? "all checks passed" : `${failed} check(s) failed`);
  process.exit(failed === 0 ? 0 : 1);
}

await main();
