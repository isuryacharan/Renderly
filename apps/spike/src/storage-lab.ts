// S1.4 storage lab. `window.lab` is what scripts/storage-e2e.mjs drives in browser storage; the
// buttons run the same code against a disk folder for the manual check
// (packages/storage/MANUAL-CHECK.md).

import "@renderly/ui/fonts.css";
import "@renderly/ui/styles.css";
import "./spike.css";
import {
  createId,
  projectSchema,
  serialiseProject,
  type Command,
  type Project,
  CURRENT_VERSION,
} from "@renderly/core";
import {
  access,
  browserRoot,
  createProject,
  listBackups,
  listProjects,
  loadProject,
  pickDiskRoot,
  readQuota,
  readText,
  recallRoot,
  rememberRoot,
  requestPersistence,
  subdir,
  writeText,
  PROJECT_FILE,
  ProjectSession,
  type AutosaveOptions,
  type Dir,
  type SessionEvent,
} from "@renderly/storage";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const DISK_KEY = "disk-root";

let session: ProjectSession | null = null;
let folder: string | null = null;
const events: SessionEvent[] = [];

function log(line: string): void {
  const el = $("log");
  el.textContent =
    `${new Date().toISOString().slice(11, 23)} ${line}\n${el.textContent ?? ""}`.slice(0, 20_000);
}

function showStatus(): void {
  const save = session?.autosave?.state;
  $("status").textContent = session
    ? `${folder} · ${session.role} · ${save ? `${save.status}, saved r${save.savedRevision}` : "read-only"} · r${session.store.revision} · "${session.store.project.name}"`
    : "No project open";
}

async function root(): Promise<Dir> {
  if (($("location") as HTMLSelectElement).value === "browser") return browserRoot();
  const handle = await recallRoot(DISK_KEY);
  if (!handle) throw new Error("Choose a folder first");
  if ((await access(handle)) !== "granted")
    throw new Error("Press Reconnect to allow access again");
  return handle;
}

function newProject(name: string, markers = 0): Project {
  const sequence = createId();
  return projectSchema().parse({
    format: "renderly",
    version: CURRENT_VERSION,
    id: createId(),
    name,
    revision: 0,
    sequences: [
      {
        id: sequence,
        name: "Main",
        width: 1920,
        height: 1080,
        fps: [30, 1],
        markers: Array.from({ length: markers }, (_, i) => ({
          id: createId(),
          t: i * 23_520_000,
          name: `Marker ${i} with a name long enough to make the file big`,
        })),
      },
    ],
  }) as Project;
}

const lab = {
  events,
  /** Empties browser storage's projects folder. */
  async reset(): Promise<void> {
    await session?.close();
    session = null;
    const opfs = await navigator.storage.getDirectory();
    await opfs.removeEntry("projects", { recursive: true }).catch(() => undefined);
  },
  async create(name: string, markers = 0): Promise<string> {
    return createProject(await root(), newProject(name, markers));
  },
  async list() {
    return listProjects(await root());
  },
  async open(name: string, autosave: AutosaveOptions = {}, flushOnHide = true): Promise<string> {
    await session?.close();
    events.length = 0;
    const dir = await subdir(await root(), name, false);
    session = await ProjectSession.open(dir, { autosave, flushOnHide });
    folder = name;
    session.on((event) => {
      events.push(event);
      if (event.type === "role") log(`role: ${event.role}`);
      if (event.type === "reloaded") log(`reloaded r${event.revision}`);
      if (event.type === "save")
        log(`${event.save.status}${event.save.error ? `: ${event.save.error}` : ""}`);
      showStatus();
    });
    session.store.subscribe(showStatus);
    showStatus();
    return session.role;
  },
  role: () => session?.role ?? "none",
  recovered: () => session?.recovered ?? null,
  status: () => session?.autosave?.state.status ?? "read-only",
  savedRevision: () => session?.autosave?.state.savedRevision ?? -1,
  revision: () => session?.store.revision ?? -1,
  name: () => session?.store.project.name ?? "",
  dispatch(command: Command) {
    if (!session) throw new Error("No project open");
    return session.store.dispatch(command);
  },
  rename(name: string) {
    return lab.dispatch({ type: "project.rename", payload: { name } });
  },
  async takeOver(): Promise<string> {
    await session?.takeOver();
    return lab.role();
  },
  async close(): Promise<void> {
    await session?.close();
    session = null;
  },
  async resolve(choice: "mine" | "disk") {
    if (!session?.autosave) throw new Error("Not the writer");
    return session.autosave.resolve(choice);
  },
  async checkOutside(): Promise<boolean> {
    return (await session?.autosave?.checkOutside()) ?? false;
  },
  async readDisk(name: string): Promise<string> {
    return (await readText(await subdir(await root(), name, false), PROJECT_FILE)) ?? "";
  },
  /** Writes project.json the way another program would: not through the session. */
  async writeOutside(name: string, projectName: string): Promise<void> {
    const dir = await subdir(await root(), name, false);
    const text = (await readText(dir, PROJECT_FILE)) ?? "";
    const project = JSON.parse(text) as Project;
    await writeText(dir, PROJECT_FILE, serialiseProject({ ...project, name: projectName }));
  },
  async load(name: string) {
    const loaded = await loadProject(await root(), name);
    return {
      ok: loaded.result.ok,
      bytes: loaded.text.length,
      name: loaded.result.ok ? loaded.result.project.name : null,
    };
  },
  async backups(name: string) {
    return listBackups(await subdir(await root(), name, false));
  },
  async restore(backup: string): Promise<void> {
    await session?.restore(backup);
  },
  async quota() {
    return { ...(await readQuota()), granted: await requestPersistence().catch(() => false) };
  },
};

Object.assign(window, { lab });

// --- buttons, for the manual check ---

async function refresh(): Promise<void> {
  const list = $("projects");
  list.replaceChildren();
  try {
    for (const entry of await lab.list()) {
      const item = document.createElement("li");
      const button = document.createElement("button");
      button.className = "r-button r-button--ghost";
      button.textContent = `${entry.meta.name} (${entry.folder}, r${entry.meta.revision})`;
      button.onclick = () => void run(() => lab.open(entry.folder));
      item.append(button);
      list.append(item);
    }
  } catch (error) {
    log(error instanceof Error ? error.message : String(error));
  }
}

async function run(action: () => Promise<unknown>): Promise<void> {
  try {
    await action();
  } catch (error) {
    log(error instanceof Error ? error.message : String(error));
  }
  showStatus();
  await refresh();
}

$("location").onchange = () => void refresh();
$("pick").onclick = () =>
  void run(async () => {
    await rememberRoot(DISK_KEY, await pickDiskRoot());
    ($("location") as HTMLSelectElement).value = "disk";
  });
$("reconnect").onclick = () =>
  void run(async () => {
    const handle = await recallRoot(DISK_KEY);
    if (!handle) throw new Error("No folder chosen yet");
    log(`access: ${await access(handle, true)}`);
  });
$("create").onclick = () =>
  void run(async () =>
    lab.open(await lab.create(($("name") as HTMLInputElement).value || "Untitled")),
  );
$("rename").onclick = () =>
  void run(async () => lab.rename(($("name") as HTMLInputElement).value || "Untitled"));
$("takeover").onclick = () => void run(() => lab.takeOver());
$("mine").onclick = () => void run(() => lab.resolve("mine"));
$("disk").onclick = () => void run(() => lab.resolve("disk"));
$("close").onclick = () => void run(() => lab.close());

showStatus();
void refresh();
Object.assign(window, { pageReadyAt: performance.now() });
