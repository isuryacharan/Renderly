# Manual check: projects in a disk folder

The automated scenarios (`pnpm test:storage`) cover browser storage. A disk folder runs the same
code on a different directory handle, but choosing a folder opens a native dialog that automation
cannot drive, and keeping access across a browser restart needs a real restart. This check covers
both. It takes about ten minutes.

Use Chrome or Edge on a desktop.

## Setup

1. Run `pnpm dev` and open http://localhost:5173/storage.html.
2. Make an empty folder somewhere, for example `Documents/Renderly check`.

## Steps

Write down **pass** or **fail** for each step. Any step that fails: note what you saw.

1. **Choose the folder.** Click **Choose folder…**, pick the empty folder, and allow editing.
   The location switches to **Disk folder**.
2. **Create a project.** Type `Disk check` in the name box and click **New project**. The status
   line reads `… · writer · saved …`.
3. **Folder layout.** In Explorer or Finder, the folder now holds `disk-check-xxxxxx/` with
   `project.json`, `project.meta.json`, and the folders `backups`, `media`, `cache` and `packs`.
4. **Autosave.** Type `Disk check 2` and click **Rename**. Within a second the status reads
   `saved`. Open `project.json` in a text editor: `"name": "Disk check 2"`.
5. **Outside change.** In the text editor, change the name to `Edited outside` and save. Back in
   the browser, click **Rename** with any name. The status reads `conflict`, and `project.json`
   still says `Edited outside`. Click **Load disk version**: the status line shows
   `"Edited outside"` and `saved`.
6. **Second tab.** Open the same page in a second tab, choose **Disk folder** in the location list
   (click **Reconnect** if it asks), and click the project. The second tab reads `follower`.
   Rename in the first tab: the second tab shows the new name within a second.
7. **Take over.** In the second tab click **Take over editing**. It reads `writer`; the first tab
   reads `follower`. Rename in the second tab: it saves.
8. **Restart.** Close every browser window (make sure the browser has fully quit), then start it
   again and open http://localhost:5173/storage.html.
9. **Reconnect.** Choose **Disk folder**. The project is listed, or the log says to press
   **Reconnect**: press it and allow access. The project is listed.
10. **Reopen.** Click the project. It opens as `writer` with the last name you saved. Rename it
    once more: the status reads `saved`, and `project.json` in the text editor shows the new name.

## Report

Send the step numbers that failed, with what you saw, or "all passed". Delete the check folder
afterwards.
