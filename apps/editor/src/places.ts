// Where projects are kept: browser storage, or a folder on disk (Chromium). The disk folder's
// handle lives in IndexedDB; after a browser restart it may need one click to reconnect.

import {
  access,
  browserRoot,
  canPickFolder,
  pickDiskRoot,
  recallRoot,
  rememberRoot,
  type Dir,
} from "@renderly/storage";

export type Place = "browser" | "disk";

export type RootState =
  | { status: "ready"; root: Dir; label: string }
  | { status: "no-folder" }
  | { status: "needs-permission"; label: string }
  | { status: "unsupported" };

const DISK_KEY = "disk-root";
const PLACE_KEY = "renderly:place";
const LAST_KEY = "renderly:last";

export async function openRoot(place: Place): Promise<RootState> {
  if (place === "browser")
    return { status: "ready", root: await browserRoot(), label: "Browser storage" };
  if (!canPickFolder()) return { status: "unsupported" };
  const handle = await recallRoot(DISK_KEY);
  if (!handle) return { status: "no-folder" };
  if ((await access(handle)) !== "granted")
    return { status: "needs-permission", label: handle.name };
  return { status: "ready", root: handle, label: handle.name };
}

/** Needs a user gesture. */
export async function chooseFolder(): Promise<void> {
  await rememberRoot(DISK_KEY, await pickDiskRoot());
}

/** Needs a user gesture. */
export async function reconnectFolder(): Promise<boolean> {
  const handle = await recallRoot(DISK_KEY);
  return handle !== null && (await access(handle, true)) === "granted";
}

function read<T>(key: string, parse: (value: unknown) => T | null): T | null {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? null : parse(JSON.parse(raw));
  } catch {
    return null;
  }
}

function write(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // private mode: not remembered
  }
}

const isPlace = (value: unknown): value is Place => value === "browser" || value === "disk";

export const savedPlace = (): Place => read(PLACE_KEY, (v) => (isPlace(v) ? v : null)) ?? "browser";
export const savePlace = (place: Place) => write(PLACE_KEY, place);

export interface LastProject {
  place: Place;
  folder: string;
  name: string;
}

export const lastProject = (): LastProject | null =>
  read(LAST_KEY, (v) => {
    const last = v as Partial<LastProject>;
    return isPlace(last.place) && typeof last.folder === "string" && typeof last.name === "string"
      ? (last as LastProject)
      : null;
  });
export const saveLastProject = (last: LastProject) => write(LAST_KEY, last);
