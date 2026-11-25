// The media bin (library panel): import by picker, folder or drop; a grid or a list of the
// project's media with thumbnail, length, picture size, codec and whether the timeline uses it;
// offline files with reconnect and relink; files this browser cannot decode, with the reason.

import type { MediaItem, Project } from "@renderly/core";
import { createProber, type Prober } from "@renderly/media";
import {
  codecName,
  importMedia,
  mediaInFolder,
  readThumbnail,
  reconnectMedia,
  relinkFromFolder,
  relinkMedia,
  resolveMedia,
  type ImportProgress,
  type ImportSource,
  type OfflineReason,
  type ProjectSession,
} from "@renderly/storage";
import {
  Button,
  Dialog,
  Icon,
  IconButton,
  MenuButton,
  Tooltip,
  useToast,
} from "@renderly/ui/react";
import { useCallback, useEffect, useMemo, useRef, useState, type DragEvent } from "react";
import { formatDuration } from "../format";

type ItemState =
  | { status: "checking" }
  | { status: "offline"; reason: OfflineReason }
  | { status: "online"; unsupported?: string };

const OFFLINE_WORDS: Record<OfflineReason, string> = {
  missing: "The file has moved or was deleted.",
  permission: "Renderly needs your permission again to read it.",
  changed: "The file at its place is a different file.",
  unlinked: "This browser has no link to the file.",
};

interface PickerWindow {
  showOpenFilePicker?(options: object): Promise<FileSystemFileHandle[]>;
  showDirectoryPicker?(options: object): Promise<FileSystemDirectoryHandle>;
}
const picker = globalThis as PickerWindow;
const PICKER_TYPES = [
  {
    description: "Video and audio",
    accept: {
      "video/*": [".mp4", ".m4v", ".mov", ".webm", ".mkv"],
      "audio/*": [".mp3", ".m4a", ".aac", ".wav", ".flac", ".ogg", ".opus"],
    },
  },
];

function remembered<T extends string>(key: string, fallback: T, allowed: readonly T[]): T {
  try {
    const value = localStorage.getItem(key);
    return allowed.includes(value as T) ? (value as T) : fallback;
  } catch {
    return fallback;
  }
}
function remember(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // private mode
  }
}

const isAbort = (error: unknown) => error instanceof DOMException && error.name === "AbortError";

export function MediaBin({
  session,
  project,
  readOnly,
}: {
  session: ProjectSession;
  project: Project;
  readOnly: boolean;
}) {
  const toast = useToast();
  const prober = useRef<Prober | null>(null);
  const getProber = () => (prober.current ??= createProber());
  useEffect(() => () => prober.current?.close(), []);

  const [view, setView] = useState(() =>
    remembered("renderly:bin-view", "grid", ["grid", "list"] as const),
  );
  const [copy, setCopy] = useState(
    () => remembered("renderly:bin-copy", "link", ["link", "copy"] as const) === "copy",
  );
  const [states, setStates] = useState<Map<string, ItemState>>(new Map());
  const [thumbs, setThumbs] = useState<Map<string, string>>(new Map());
  const [progress, setProgress] = useState<ImportProgress | null>(null);
  const [dropping, setDropping] = useState(false);
  const [removing, setRemoving] = useState<{ item: MediaItem; clips: number } | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const checked = useRef(new Set<string>());
  const projectId = project.id;

  const usage = useMemo(() => {
    const count = new Map<string, number>();
    for (const sequence of project.sequences)
      for (const track of sequence.tracks)
        for (const clip of track.clips)
          if (clip.media !== undefined) count.set(clip.media, (count.get(clip.media) ?? 0) + 1);
    return count;
  }, [project.sequences]);

  /** Works out (again) whether each item's file can be read, and whether it can be decoded. */
  const check = useCallback(
    async (items: readonly MediaItem[]) => {
      for (const item of items) {
        const resolved = await resolveMedia(session.dir, projectId, item).catch(
          (): { status: "offline"; reason: OfflineReason } => ({
            status: "offline",
            reason: "missing",
          }),
        );
        let state: ItemState;
        if (resolved.status === "offline") state = resolved;
        else {
          const probed = await getProber().probe(resolved.file, { thumbnail: false });
          const unsupported = !probed.ok
            ? probed.detail
            : probed.video && !probed.video.decodable
              ? `This browser cannot decode ${codecName(probed.video.codec)} video.`
              : !probed.video && probed.audio && !probed.audio.decodable
                ? `This browser cannot decode ${codecName(probed.audio.codec)} audio.`
                : undefined;
          state = unsupported ? { status: "online", unsupported } : { status: "online" };
        }
        setStates((all) => new Map(all).set(item.id, state));
        if (!thumbs.has(item.id)) {
          const file = await readThumbnail(session.dir, item.id);
          if (file) setThumbs((all) => new Map(all).set(item.id, URL.createObjectURL(file)));
        }
      }
    },
    [session.dir, projectId, thumbs],
  );

  // Check items as they appear (on open, and after each import).
  useEffect(() => {
    const fresh = project.media.filter((m) => !checked.current.has(m.id));
    for (const item of fresh) checked.current.add(item.id);
    if (fresh.length > 0) void check(fresh);
  }, [project.media, check]);

  useEffect(() => () => thumbs.forEach((url) => URL.revokeObjectURL(url)), [thumbs]);

  const run = async (sources: ImportSource[]) => {
    if (sources.length === 0) return;
    try {
      const result = await importMedia(session, sources, (file) => getProber().probe(file), {
        copy,
        onProgress: setProgress,
      });
      if (result.imported.length > 0)
        toast(`Imported ${result.imported.length} file${result.imported.length === 1 ? "" : "s"}`, {
          action: { label: "Undo", onSelect: () => session.store.undo() },
        });
      for (const failure of result.failed)
        toast(`${failure.name}: ${failure.reason}`, { kind: "danger" });
    } catch (error) {
      if (!isAbort(error))
        toast(error instanceof Error ? error.message : String(error), { kind: "danger" });
    } finally {
      setProgress(null);
    }
  };

  const importFiles = async () => {
    if (!picker.showOpenFilePicker) return fileInput.current?.click();
    try {
      const handles = await picker.showOpenFilePicker({
        multiple: true,
        types: PICKER_TYPES,
        id: "renderly-media",
      });
      await run(handles.map((handle) => ({ handle })));
    } catch (error) {
      if (!isAbort(error))
        toast(error instanceof Error ? error.message : String(error), { kind: "danger" });
    }
  };

  const importFolder = async () => {
    try {
      const folder = await picker.showDirectoryPicker?.({ mode: "read", id: "renderly-media" });
      if (folder) await run((await mediaInFolder(folder)).map((handle) => ({ handle })));
    } catch (error) {
      if (!isAbort(error))
        toast(error instanceof Error ? error.message : String(error), { kind: "danger" });
    }
  };

  const onDrop = async (event: DragEvent) => {
    event.preventDefault();
    setDropping(false);
    if (readOnly) return;
    const sources: ImportSource[] = [];
    const entries = [...event.dataTransfer.items].filter((item) => item.kind === "file");
    const handles = await Promise.all(
      entries.map(
        (item) =>
          (
            item as DataTransferItem & {
              getAsFileSystemHandle?(): Promise<FileSystemHandle | null>;
            }
          ).getAsFileSystemHandle?.() ?? null,
      ),
    );
    for (let i = 0; i < entries.length; i++) {
      const handle = handles[i];
      if (handle?.kind === "directory")
        sources.push(
          ...(await mediaInFolder(handle as FileSystemDirectoryHandle)).map((h) => ({ handle: h })),
        );
      else if (handle?.kind === "file") sources.push({ handle: handle as FileSystemFileHandle });
      else {
        const file = entries[i]?.getAsFile();
        if (file) sources.push({ file });
      }
    }
    await run(sources);
  };

  const offline = project.media.filter((m) => states.get(m.id)?.status === "offline");
  const needPermission = offline.filter((m) => {
    const s = states.get(m.id);
    return s?.status === "offline" && s.reason === "permission";
  });

  const recheck = async (items: readonly MediaItem[]) => {
    setStates((all) => {
      const next = new Map(all);
      for (const item of items) next.set(item.id, { status: "checking" });
      return next;
    });
    await check(items);
  };

  const reconnect = async () => {
    await reconnectMedia(projectId, needPermission);
    await recheck(needPermission);
  };

  const findInFolder = async () => {
    try {
      const folder = await picker.showDirectoryPicker?.({ mode: "read", id: "renderly-relink" });
      if (!folder) return;
      const relinked = await relinkFromFolder(projectId, offline, folder);
      toast(
        relinked.length > 0
          ? `Relinked ${relinked.length} of ${offline.length} files`
          : "None of the offline files are in that folder",
      );
      await recheck(offline);
    } catch (error) {
      if (!isAbort(error))
        toast(error instanceof Error ? error.message : String(error), { kind: "danger" });
    }
  };

  const relinkOne = async (item: MediaItem) => {
    try {
      const [handle] =
        (await picker.showOpenFilePicker?.({ types: PICKER_TYPES, id: "renderly-relink" })) ?? [];
      if (!handle) return;
      const result = await relinkMedia(projectId, item, handle);
      if (!result.ok)
        toast(`${handle.name} is not the same file as ${item.name}`, { kind: "danger" });
      await recheck([item]);
    } catch (error) {
      if (!isAbort(error))
        toast(error instanceof Error ? error.message : String(error), { kind: "danger" });
    }
  };

  const remove = (item: MediaItem, withClips: boolean) => {
    const result = session.store.dispatch({
      type: "media.remove",
      payload: { ids: [item.id], withClips },
    });
    if (result.ok)
      toast(`Removed ${item.name}`, {
        action: { label: "Undo", onSelect: () => session.store.undo() },
      });
    else toast(result.error.message, { kind: "danger" });
  };

  return (
    <div
      className={`r-bin${dropping ? " r-bin__drop" : ""}`}
      onDragOver={(event) => {
        if (readOnly || !event.dataTransfer.types.includes("Files")) return;
        event.preventDefault();
        setDropping(true);
      }}
      onDragLeave={() => setDropping(false)}
      onDrop={(event) => void onDrop(event)}
    >
      <div className="r-bin__toolbar">
        <Button
          size="sm"
          disabled={readOnly || progress !== null}
          onClick={() => void importFiles()}
        >
          <Icon name="import" />
          Import
        </Button>
        {picker.showDirectoryPicker && (
          <IconButton
            icon="folder"
            label="Import a folder"
            size="sm"
            disabled={readOnly || progress !== null}
            onClick={() => void importFolder()}
          />
        )}
        <label className="r-choice">
          <input
            type="checkbox"
            className="r-checkbox"
            checked={copy}
            onChange={(event) => {
              setCopy(event.target.checked);
              remember("renderly:bin-copy", event.target.checked ? "copy" : "link");
            }}
          />
          Copy into project
        </label>
        <span className="r-toolbar__spacer" />
        <div className="r-segmented r-segmented--icons" role="group" aria-label="Show media as">
          {(["grid", "list"] as const).map((option) => (
            <Tooltip key={option} label={option === "grid" ? "Grid" : "List"}>
              <button
                type="button"
                aria-label={option === "grid" ? "Grid" : "List"}
                aria-pressed={view === option}
                onClick={() => {
                  setView(option);
                  remember("renderly:bin-view", option);
                }}
              >
                <Icon name={option} />
              </button>
            </Tooltip>
          ))}
        </div>
        <input
          ref={fileInput}
          type="file"
          multiple
          accept="video/*,audio/*"
          hidden
          onChange={(event) => {
            const files = [...(event.target.files ?? [])];
            event.target.value = "";
            void run(files.map((file) => ({ file })));
          }}
        />
      </div>

      {progress && (
        <p className="r-bin__notice" role="status">
          {progress.copying
            ? `Copying ${progress.copying.name}: ${Math.round(progress.copying.share * 100)}%`
            : `Importing ${progress.done} of ${progress.total}…`}
        </p>
      )}

      {offline.length > 0 && (
        <div className="r-bin__notice" role="status">
          <span className="r-badge r-badge--warning">
            <Icon name="warning" />
            Offline
          </span>
          <span>
            {offline.length} file{offline.length === 1 ? " is" : "s are"} offline.
          </span>
          {needPermission.length > 0 && (
            <Button size="sm" onClick={() => void reconnect()}>
              <Icon name="reconnect" />
              Reconnect
            </Button>
          )}
          {offline.length > needPermission.length && picker.showDirectoryPicker && !readOnly && (
            <Button size="sm" onClick={() => void findInFolder()}>
              <Icon name="relink" />
              Find in a folder…
            </Button>
          )}
          {offline.length > needPermission.length && !picker.showDirectoryPicker && (
            <span>To relink them, open this project in Chrome or Edge on a desktop.</span>
          )}
        </div>
      )}

      {project.media.length === 0 ? (
        <p className="r-panel__empty">
          {readOnly ? "No media yet." : "No media yet. Import files, or drop them here."}
        </p>
      ) : (
        <ul className={`r-bin__items r-bin__items--${view}`} aria-label="Media">
          {project.media.map((item) => {
            const state = states.get(item.id) ?? { status: "checking" as const };
            const used = usage.get(item.id) ?? 0;
            const codec = item.video
              ? codecName(item.video.codec)
              : item.audio
                ? codecName(item.audio.codec)
                : "";
            const thumb = thumbs.get(item.id);
            return (
              <li key={item.id} className="r-bin__item">
                <div className="r-bin__thumb">
                  {thumb && state.status !== "offline" ? (
                    <img src={thumb} alt="" />
                  ) : (
                    <Icon name={item.video ? "video" : "audio"} />
                  )}
                  {view === "grid" && (
                    <span className="r-bin__duration">{formatDuration(item.duration)}</span>
                  )}
                </div>
                <div className="r-bin__text">
                  <span className="r-bin__name" title={item.name}>
                    {item.name}
                  </span>
                  <span className="r-bin__facts">
                    {view === "list" && (
                      <span className="r-bin__detail">{formatDuration(item.duration)}</span>
                    )}
                    {item.video && (
                      <span className="r-bin__detail">
                        {item.video.rotation % 180
                          ? `${item.video.height}×${item.video.width}`
                          : `${item.video.width}×${item.video.height}`}
                      </span>
                    )}
                    {codec && <span className="r-badge">{codec}</span>}
                    {used > 0 && (
                      <span
                        className="r-badge"
                        title={`Used by ${used} clip${used === 1 ? "" : "s"}`}
                      >
                        <Icon name="check" />
                        In timeline
                      </span>
                    )}
                    {state.status === "offline" && (
                      <span
                        className="r-badge r-badge--warning"
                        title={OFFLINE_WORDS[state.reason]}
                      >
                        <Icon name="warning" />
                        Offline
                      </span>
                    )}
                    {state.status === "online" && state.unsupported && (
                      <span className="r-badge r-badge--danger" title={state.unsupported}>
                        <Icon name="error" />
                        Cannot play
                      </span>
                    )}
                  </span>
                </div>
                {!readOnly && (
                  <div className="r-bin__menu">
                    <MenuButton
                      label={<Icon name="more" />}
                      aria-label={`More actions for ${item.name}`}
                      tooltip="More actions"
                      className="r-button r-button--icon r-button--sm"
                      items={[
                        ...(state.status === "offline" && state.reason === "permission"
                          ? [{ label: "Reconnect", onSelect: () => void reconnect() }]
                          : state.status === "offline" && picker.showOpenFilePicker
                            ? [{ label: "Relink…", onSelect: () => void relinkOne(item) }]
                            : []),
                        {
                          label: "Remove from project",
                          danger: true,
                          onSelect: () =>
                            used > 0 ? setRemoving({ item, clips: used }) : remove(item, false),
                        },
                      ]}
                    />
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}

      <Dialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title="Remove from the project?"
        actions={
          <>
            <Button onClick={() => setRemoving(null)}>Cancel</Button>
            <Button
              variant="primary"
              onClick={() => {
                if (removing) remove(removing.item, true);
                setRemoving(null);
              }}
            >
              Remove
            </Button>
          </>
        }
      >
        <p className="r-dialog__text">
          {removing?.item.name} is used by {removing?.clips} clip{removing?.clips === 1 ? "" : "s"}{" "}
          on the timeline. They will be removed too. The file itself stays where it is.
        </p>
      </Dialog>
    </div>
  );
}
