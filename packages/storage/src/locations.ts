// Where projects live. Two locations, one implementation, because both are directory handles:
// - browser storage (OPFS): every current browser, no prompts, private to the site;
// - a disk folder (Chromium): real files the user can see, back up and sync. Its handle is kept in
//   IndexedDB; after a browser restart it may need one click to reconnect (a user gesture).

import { subdir, type Dir } from "./fs";

export type LocationKind = "browser" | "disk";

/** Browser storage: a `projects` folder in the origin's private file system. */
export async function browserRoot(): Promise<Dir> {
  return subdir(await navigator.storage.getDirectory(), "projects");
}

// Chromium-only parts of the File System Access API, not in TypeScript's DOM library.
type Permission = "granted" | "denied" | "prompt";
interface PermissionHandle {
  queryPermission?(options: { mode: "readwrite" }): Promise<Permission>;
  requestPermission?(options: { mode: "readwrite" }): Promise<Permission>;
}
interface PickerWindow {
  showDirectoryPicker?(options: { id?: string; mode?: "readwrite" }): Promise<Dir>;
}

export const canPickFolder = (): boolean =>
  typeof (globalThis as PickerWindow).showDirectoryPicker === "function";

/** Asks the user for a disk folder (needs a user gesture). */
export async function pickDiskRoot(): Promise<Dir> {
  const picker = (globalThis as PickerWindow).showDirectoryPicker;
  if (!picker) throw new Error("This browser cannot open folders on disk; use browser storage");
  return picker.call(globalThis, { id: "renderly-projects", mode: "readwrite" });
}

/**
 * Whether the app may read and write a handle now. With `request`, asks the user (needs a user
 * gesture: the "Reconnect" button after a restart). Browsers without the permission API (OPFS
 * everywhere) always have access.
 */
export async function access(handle: Dir, request = false): Promise<Permission> {
  const h = handle as Dir & PermissionHandle;
  const ask = request ? h.requestPermission : h.queryPermission;
  if (!ask) return "granted";
  return ask.call(h, { mode: "readwrite" });
}

// --- remembered handles (IndexedDB: the only storage that can hold a directory handle) ---

const DB = "renderly";
const STORE = "handles";

function database(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("IndexedDB failed to open"));
  });
}

async function transact<T>(
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const db = await database();
  try {
    return await new Promise<T>((resolve, reject) => {
      const request = run(db.transaction(STORE, mode).objectStore(STORE));
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error ?? new Error("IndexedDB request failed"));
    });
  } finally {
    db.close();
  }
}

export async function rememberRoot(key: string, handle: Dir): Promise<void> {
  await transact("readwrite", (store) => store.put(handle, key));
}

export async function recallRoot(key: string): Promise<Dir | null> {
  const handle = await transact<unknown>("readonly", (store) => store.get(key));
  return handle instanceof FileSystemDirectoryHandle ? handle : null;
}

export async function forgetRoot(key: string): Promise<void> {
  await transact("readwrite", (store) => store.delete(key));
}

// --- persistence and quota ---

/** Asks the browser not to evict browser storage under pressure. Returns whether it agreed. */
export async function requestPersistence(): Promise<boolean> {
  if (await navigator.storage.persisted()) return true;
  return navigator.storage.persist();
}

export interface Quota {
  /** Bytes used by this origin (browser storage, IndexedDB, caches). */
  usage: number;
  /** Bytes the browser will let this origin use. */
  quota: number;
  persisted: boolean;
}

export async function readQuota(): Promise<Quota> {
  const estimate = await navigator.storage.estimate();
  return {
    usage: estimate.usage ?? 0,
    quota: estimate.quota ?? 0,
    persisted: await navigator.storage.persisted(),
  };
}
