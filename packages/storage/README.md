# @renderly/storage

Projects as folders, in browser storage (OPFS, every browser) or a folder on disk (Chromium).
Both are directory handles, so there is one implementation.

Status: M1, slice S1.4.

```
holiday-edit-d27a5e/
  project.json        the edit
  project.meta.json   name, duration, modified, revision: all the home screen reads
  backups/            rolling copies of project.json
  media/              copied media (linked media stays where it is)
  cache/              thumbnails, waveforms, proxies: safe to delete
  packs/              pack items the project uses
```

## Use

```ts
import { browserRoot, createProject, listProjects, ProjectSession, subdir } from "@renderly/storage";

const root = await browserRoot(); // or a disk folder: pickDiskRoot(), then rememberRoot()
const folder = await createProject(root, project);
const session = await ProjectSession.open(await subdir(root, folder, false));
session.role;                    // "writer", or "follower" if another tab is editing it
session.store.dispatch(command); // edits save themselves
session.on((event) => { /* role changes, save status, reloads */ });
await session.close();
```

## What it guarantees

- **Autosave** (`autosave.ts`):
  - Writes run 400 ms after the last edit.
  - There is never more than one write at a time.
  - Each write is atomic: the file is replaced only when the write completes, so a crash mid-save
    leaves the previous version.
  - Failed writes are retried after 1 s, 2 s, 4 s and so on, up to every 30 s.
  - Status events report `saved`, `unsaved`, `saving`, `error` or `conflict`.
- **Unload**:
  - When the tab is hidden or the page unloads, the writer starts a save.
  - A save started during unload often doesn't finish, so the writer also keeps a copy of unsaved
    edits in `localStorage`, synchronously.
  - The next writer adopts the copy only if the disk is still exactly what the edits were based
    on (same revision and modification stamp).
  - Projects too big for `localStorage` (about 5 MB) rely on the save alone.
- **Outside changes are never overwritten silently.**
  - Before each write, autosave checks that `project.json` is what it last wrote. Where
    `FileSystemObserver` exists, changes are also noticed as they happen.
  - On a change, the status becomes `conflict` until the user picks: `resolve("mine")` or
    `resolve("disk")`.
  - The revision never goes backwards, even when the disk version is older.
- **One writer per project across tabs.**
  - The writer holds a Web Lock named after the project id. Other tabs follow read-only and reload
    after each save (BroadcastChannel).
  - `takeOver()` asks the writer to save, then steals the lock.
  - When the writer closes, a waiting tab becomes the writer by itself.
- **Backups**:
  - A save backs up `project.json` when the last backup is older than 5 minutes.
  - Retention keeps the newest 20, plus the newest of each day for 14 days.
  - `session.restore(name)` makes a backup the current version.
- **Quota**: `requestPersistence()` and `readQuota()`.

## Tests

- `pnpm test` runs the pure parts: retention, backoff and folder names.
- `pnpm test:storage` runs eleven scenarios in a real browser, in browser storage:
  - saving, and reloading;
  - reloading inside the debounce window, with and without the unload save;
  - a stale kept copy being dropped;
  - two tabs, take-over with an unsaved edit, and the writer closing;
  - an outside write;
  - six renderer crashes, each in the middle of writing a 39 MB `project.json`;
  - backups, and the quota read-out.
- [MANUAL-CHECK.md](MANUAL-CHECK.md) is the disk-folder check, which needs a person: a native
  folder dialog and a browser restart.
