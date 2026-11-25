// Small helpers over a FileSystemDirectoryHandle. A disk folder and browser storage (OPFS) are
// both directory handles, so everything above this file is one implementation.

export type Dir = FileSystemDirectoryHandle;

const isNotFound = (error: unknown) =>
  error instanceof DOMException &&
  (error.name === "NotFoundError" || error.name === "TypeMismatchError");

export async function subdir(dir: Dir, name: string, create = true): Promise<Dir> {
  return dir.getDirectoryHandle(name, { create });
}

export async function readText(dir: Dir, name: string): Promise<string | null> {
  try {
    const file = await (await dir.getFileHandle(name)).getFile();
    return await file.text();
  } catch (error) {
    if (isNotFound(error)) return null;
    throw error;
  }
}

/** When the file last changed (ms), or null when it does not exist. */
export async function fileStamp(dir: Dir, name: string): Promise<number | null> {
  try {
    return (await (await dir.getFileHandle(name)).getFile()).lastModified;
  } catch (error) {
    if (isNotFound(error)) return null;
    throw error;
  }
}

/**
 * Writes a whole file atomically: `createWritable()` writes to a swap file that replaces the
 * original only on `close()`, so a crash mid-write leaves the previous version. Returns the new
 * modification stamp.
 */
export async function writeText(dir: Dir, name: string, text: string): Promise<number> {
  const handle = await dir.getFileHandle(name, { create: true });
  const writable = await handle.createWritable();
  try {
    await writable.write(text);
    await writable.close();
  } catch (error) {
    await writable.abort().catch(() => undefined);
    throw error;
  }
  return (await handle.getFile()).lastModified;
}

export interface Entry {
  name: string;
  kind: "file" | "directory";
}

export async function list(dir: Dir): Promise<Entry[]> {
  const entries: Entry[] = [];
  for await (const handle of dir.values()) entries.push({ name: handle.name, kind: handle.kind });
  return entries.sort((a, b) => a.name.localeCompare(b.name));
}

export async function remove(dir: Dir, name: string): Promise<void> {
  try {
    await dir.removeEntry(name, { recursive: true });
  } catch (error) {
    if (!isNotFound(error)) throw error;
  }
}
