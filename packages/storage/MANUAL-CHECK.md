# Manual checks: disk folders and media files

The automated scenarios (`pnpm test:storage`, `pnpm test:media`) cover browser storage. Real
folders and files on disk run the same code on different handles, but choosing them opens native
dialogs that automation cannot drive, and keeping access across a browser restart needs a real
restart. Part 1 checks projects in a disk folder (S1.4); part 2 checks media files on disk (S1.6).
Together they take about twenty minutes.

Use Chrome or Edge on a desktop.

## Part 1: projects in a disk folder

### Setup

1. Run `pnpm dev` and open http://localhost:5173/storage.html.
2. Make an empty folder somewhere, for example `Documents/Renderly check`.

### Steps

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

## Part 2: media files on disk

### Setup

1. Make a folder, for example `Documents/Renderly media check`, and copy two or three short video
   files into it (clips from a phone are fine).
2. Run `pnpm dev:editor` and open http://localhost:5180/. Create a project (browser storage is
   fine) and open it.

### Steps

11. **Import.** In the Library panel click **Import**, pick two of the files. Both appear with a
    picture, their length, picture size and codec. "Copy into project" is off, so they are linked.
12. **Drop.** Drag the third file from Explorer or Finder onto the Library panel. It is imported.
13. **Import a folder.** Click the folder button next to **Import** and pick the folder. Its files
    are imported again (as new items).
14. **Move.** In Explorer or Finder, move one of the files into a new subfolder `moved`. Reload the
    page. Every item made from that file shows **Offline**, and the panel says how many files are
    offline.
15. **Relink.** Click **Find in a folder…** and pick `Renderly media check`. A message says the
    files were relinked; the **Offline** badges and the pictures are back.
16. **Restart.** Quit the browser completely, start it again, and open
    http://localhost:5180/. Click **Continue** to open the project.
17. **One click.** If the files show **Offline** with a **Reconnect** button, click it once and allow
    access: every file comes back. If they are already online (the browser kept the permission),
    write "no prompt": that also passes.

## Report

Send the step numbers that failed, with what you saw, or "all passed". Delete the check folders
afterwards.
