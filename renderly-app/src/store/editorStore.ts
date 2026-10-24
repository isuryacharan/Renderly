import { create } from "zustand";
import { applyPatch, type Operation } from "fast-json-patch";
import * as ipc from "../lib/ipc";
import { hoverStack, initialParams } from "../lib/effects";
import { ensurePreviewPackItems } from "../preview/previewBridge";
import { normalizeFps, snapToFrameUs, usToSecs } from "../lib/time";
import {
  addAdjustmentLayer,
  addCaption,
  addClip,
  addMarkers,
  addText,
  addTrack,
  bodyArg,
  nestClips,
  setTrackFlags,
  unnestClip,
} from "../lib/commands";
import { DEFAULT_TEXT_STYLE_REF, magnetTargetTrack, titleStyleFor } from "../lib/text";
import { timelineDurationUs, TRACK_LABEL_W, maxScrollX, maxScrollY, clipLeft } from "../timeline/layout";
import {
  activeTracks,
  containerView,
  newTrackKindForDrop,
  normalizeSequencePath,
  pathAfterOpen,
  pathForBreadcrumb,
} from "../lib/container";
import {
  clipDurationUs,
  mediaDurationUs,
  sequenceMediaName,
  type Clip,
  type MediaKind,
  type Project,
  type Selection,
  type Track,
  type TrackKind,
} from "../lib/types";

export type ToolMode = "select" | "razor" | "mask";
export type LeftTab =
  | "media"
  | "audio"
  | "text"
  | "stickers"
  | "effects"
  | "transitions"
  | "filters"
  | "adjustment"
  | "extensions";
export type ToastKind = "info" | "success" | "error";

export interface ToastItem {
  id: number;
  message: string;
  kind: ToastKind;
}

let nextToastId = 1;

/// Length of a title added from the panel / shortcut (µs).
const DEFAULT_TITLE_US = 3_000_000;

/// Mutation ids this tab has issued to the backend but hasn't finished applying yet.
/// `dispatch`/`dispatchBatch`/`undo`/`redo` apply the returned JSON Patch themselves;
/// the `project:changed` listener (wired in `connectStoreToBackendEvents`) checks this
/// set and skips its refetch when it sees one of these ids echoed back — otherwise every
/// edit did a redundant full `get_project` (B2). Anything else (a different id, or none)
/// means some other writer changed the project — MCP, another window — and must still
/// trigger the listener's refetch. Plain module-level `Set`, not store state.
const selfMutationIds = new Set<string>();

/** Clips currently showing a hover-preview effect (see `previewEffectOnSelection`). */
let effectPreviewTargets: string[] = [];

/** Options for `dispatch`/`dispatchBatch`. `mergeKey` coalesces consecutive edits with the
 *  same key into one undo step (docs/architecture.md "Coalescing"). */
export interface DispatchOptions {
  mergeKey?: string;
}

/// Apply an RFC-6902 patch from a mutation result onto the current store project.
/// Returns false (and leaves state untouched) on failure so the caller can fall back to
/// a full `refetchProject`.
function applyProjectPatch(
  get: () => EditorStore,
  set: (partial: Partial<EditorStore>) => void,
  revision: number,
  patch: ipc.JsonPatchOp[],
): boolean {
  const current = get().project;
  if (!current) return false;
  try {
    const clone = structuredClone(current) as Project;
    // `clone` is a private, disposable copy (from structuredClone above), so mutating it
    // in place is safe — and required: with `mutateDocument=false`, fast-json-patch
    // clones the document *itself* internally and returns the patched result via
    // `.newDocument`, leaving the `clone` we pass in untouched. That previously meant
    // this function reported success (no throw) while silently keeping `clone` — and
    // therefore the store's `project` — identical in content to the pre-patch state,
    // just under a new object reference. React re-rendered (new reference), but with
    // stale data, which is exactly bug 1/2's "doesn't show until reopen" symptom.
    applyPatch(clone as object, patch as Operation[], true, true);
    set({ project: clone, clientRevision: revision });
    return true;
  } catch (e) {
    console.warn("apply project patch failed; falling back to full fetch:", e);
    return false;
  }
}

export interface ThumbnailAsset {
  stripUrl: string;
  /// `null` until the browser finishes loading `stripUrl` — renderer.ts skips drawing
  /// (not an error) while this is null, and the store patches it in once `Image.onload`
  /// fires, which naturally triggers a redraw via React re-rendering subscribers of
  /// `mediaAssets`.
  image: HTMLImageElement | null;
  cols: number;
  rows: number;
  tileWidth: number;
  tileHeight: number;
  /** Microseconds between thumbnail tiles (source time). */
  intervalUs: number;
}

export interface WaveformAsset {
  peaks: number[];
  /** Microseconds of audio per peak bucket. */
  bucketUs: number;
}

export interface MediaAssetEntry {
  thumbnails?: ThumbnailAsset;
  waveform?: WaveformAsset;
}

export interface DragGhost {
  mediaId: string;
  kind: MediaKind;
  durationUs: number;
  startUs: number;
  /// Index into the open container's tracks (`activeTracks`) to drop into, or `null` to auto-create a new track (drop
  /// above the topmost overlay lane or below the bottommost lane — see
  /// `layout.dropTargetAtY`).
  trackIndex: number | null;
  /// Visual row to render the ghost at — `-1` for "new track above everything", the
  /// display row count for "new track below everything", otherwise a real row index. See
  /// `layout.dropTargetAtY`.
  row: number;
  /// Whether `trackIndex`/`row` is a real, kind-compatible drop target — drives ghost
  /// color (valid vs. invalid) the same way CapCut shows a red ghost over an incompatible
  /// track.
  valid: boolean;
}

export interface TransitionDropTarget {
  trackId: string;
  clipId: string;
  valid: boolean;
}

export interface EditorStore {
  project: Project | null;
  projectPath: string | null;
  /// B3: last applied revision from get_project / patch responses.
  clientRevision: number;
  playhead: number;
  playing: boolean;
  /**
   * Sequence open in the timeline (schema v7); null = the root timeline. Read tracks through
   * `activeTracks(project, activeSequenceId)` / `selectActiveView` (lib/container.ts), never
   * `project.tracks`, for anything that edits or shows "the timeline". Always the last entry
   * of `sequencePath`.
   */
  activeSequenceId: string | null;
  /** Sequences opened from the root, outermost first — the breadcrumb trail. */
  sequencePath: string[];
  /** Primary (focused) selection — inspector / trim handles. */
  selection: Selection | null;
  /** All selected clips including primary (`selections[0]` === selection when non-empty). */
  selections: Selection[];
  toolMode: ToolMode;
  snapEnabled: boolean;
  pxPerSec: number;
  /** Horizontal scroll of the timeline canvas (px). */
  scrollX: number;
  /** Vertical scroll of track lanes (px). */
  scrollY: number;
  leftTab: LeftTab;
  canUndo: boolean;
  canRedo: boolean;
  toasts: ToastItem[];
  importBusy: boolean;
  clipboard: { items: { clip: Clip; trackKind: TrackKind }[] } | null;
  dragGhost: DragGhost | null;
  /** C4: junction highlighted while dragging a transition onto the timeline. */
  transitionDropTarget: TransitionDropTarget | null;
  contextMenu: { x: number; y: number; trackId: string; clipId: string; atUs: number } | null;
  /** Right-click menu of a ruler marker (rename / colour / delete). */
  markerMenu: { x: number; y: number; markerId: string } | null;
  snapGuideUs: number | null;
  /// True while a timeline mouse drag (move/trim) is in progress — set by
  /// `timeline/interactions.ts`. Lets `project:changed`'s handler skip refetching mid-drag
  /// so a concurrent backend event (e.g. a slow GenerateVoiceover/GenerateCaptions/Export
  /// started before the drag began landing) doesn't overwrite the drag's local optimistic
  /// mutation and yank the clip back to its pre-drag position mid-gesture. The eventual
  /// mouseup commit's own `dispatch`/`dispatchBatch` call refetches afterward regardless,
  /// so this only defers the refresh, never skips it.
  isDragging: boolean;
  /// Thumbnail strips / waveform peaks per media id, populated from `media:thumbnails-
  /// ready` / `media:waveform-ready` events (fired automatically by the backend on
  /// import and on project open — nothing here triggers generation itself).
  mediaAssets: Record<string, MediaAssetEntry>;
  /// Latest ~1Hz averaged sample from the playback loop's perf HUD (A0), `null` before
  /// the first sample arrives or once playback stops. `perfHudVisible` is a display
  /// toggle (Ctrl+Shift+P) independent of whether a sample has arrived yet.
  perfSample: ipc.PlaybackPerfPayload | null;
  perfHudVisible: boolean;
  /// Bumped when the UI theme cycles so canvas consumers (TimelineCanvas) re-read
  /// `getTimelineTheme()` after `resetTimelineThemeCache()` (D2).
  themeEpoch: number;
  /// G2: auto-save status for the TopBar indicator. Backend already persists on every
  /// edit via `commit_project`; this just mirrors that honesty to the UI.
  saveStatus: "saved" | "saving" | null;

  toast(message: string, kind?: ToastKind): void;
  dismissToast(id: number): void;
  select(sel: Selection | null): void;
  toggleSelect(sel: Selection): void;
  setSelections(sels: Selection[]): void;
  /**
   * Open a nested sequence in the timeline (stops playback, playhead to its start). The path
   * grows by one level (the sequence is being entered from the open container, e.g. by
   * double-clicking a nested clip); `fromRoot` starts a fresh path instead - for opening a
   * sequence from the media panel, where it is not necessarily a child of the open container.
   */
  openSequence(sequenceId: string, opts?: { fromRoot?: boolean }): Promise<void>;
  /** Leave the open sequence for its parent (the root when it was opened from there). */
  closeSequence(): Promise<void>;
  /** Jump to a breadcrumb entry; `null` = the root timeline. */
  navigateToSequence(sequenceId: string | null): Promise<void>;
  setTool(tool: ToolMode): void;
  setSnap(enabled: boolean): void;
  setZoom(px: number): void;
  fitZoom(): void;
  setScroll(scrollX: number, scrollY?: number): void;
  panBy(dx: number, dy: number): void;
  ensurePlayheadVisible(): void;
  setDragGhost(ghost: DragGhost | null): void;
  setTransitionDropTarget(target: TransitionDropTarget | null): void;
  dropMediaOnTimeline(): Promise<void>;
  openContextMenu(x: number, y: number, trackId: string, clipId: string, atUs: number): void;
  closeContextMenu(): void;
  openMarkerMenu(x: number, y: number, markerId: string): void;
  closeMarkerMenu(): void;
  setSnapGuide(us: number | null): void;
  setLeftTab(tab: LeftTab): void;
  setPlayheadLocal(us: number): void;
  setDragging(dragging: boolean): void;
  onThumbnailsReady(payload: ipc.ThumbnailsReadyPayload): void;
  onWaveformReady(payload: ipc.WaveformReadyPayload): void;

  refetchProject(): Promise<void>;
  loadProjectFromPath(path: string): Promise<void>;
  quickStart(): Promise<boolean>;
  createNewProject(path: string, name: string): Promise<void>;
  saveProject(): Promise<void>;
  closeProject(): Promise<void>;
  dispatch(command: Record<string, unknown>, quiet?: boolean, opts?: DispatchOptions): Promise<boolean>;
  dispatchBatch(
    commands: Record<string, unknown>[],
    quiet?: boolean,
    opts?: DispatchOptions,
  ): Promise<boolean>;
  undo(): Promise<void>;
  redo(): Promise<void>;
  pruneStaleSelection(): void;
  /** Render-only overrides by clip id (live drags, hover preview). Mirrors the backend's set;
   *  never saved and never in undo. See docs/architecture.md "Preview overrides". */
  previewOverrides: Record<string, ipc.PreviewOverride>;
  setPreviewOverride(clipId: string, over: ipc.PreviewOverride): void;
  clearPreviewOverride(clipId: string): void;
  /** Hover preview: show catalogue item `entry` (effect or filter) on the selected clips,
   *  with its `initial` params, without a command or undo record. */
  previewEffectOnSelection(entry: Pick<ipc.CatalogueEntry, "ref" | "params">): void;
  /** Hover preview of a transition on the selected video clips' outgoing junction. */
  previewTransitionOnSelection(
    entry: Pick<ipc.CatalogueEntry, "ref" | "params">,
    durationUs: number,
  ): void;
  clearEffectPreview(): void;
  /** Close the backend's merge gesture (call on mouse-up or key-up after a live edit). */
  endGesture(): void;

  copySelection(): void;
  pasteAtPlayhead(): Promise<void>;
  duplicateSelection(): Promise<void>;
  /** `NestClips` on the current selection (all clips must share one container). */
  nestSelection(): Promise<void>;
  /** `UnnestClip` on a nested clip (defaults to the primary selection); errors surface as toasts. */
  unnestClip(trackId?: string, clipId?: string): Promise<void>;
  /**
   * `AddAdjustmentLayer` of `durationUs` (default 5 s) at the playhead on `trackId`, else the
   * selected video track, else the topmost video track (created when the container has none).
   * Selects the new layer. An overlapping clip surfaces the engine's overlap error.
   */
  addAdjustmentLayerAtPlayhead(trackId?: string, durationUs?: number): Promise<void>;

  /**
   * `AddText` of a title at the playhead (default 3 s): on the selected non-magnetic video
   * track, else the first non-magnetic video track with room, else a new "Titles" track
   * (above a new main track when the container has no video track yet). Selects the title.
   */
  addTitleAtPlayhead(
    text?: string,
    opts?: { styleRef?: string; durationUs?: number },
  ): Promise<void>;
  /** `AddMarkers` at the playhead on the root timeline; selects nothing, toasts the result. */
  addMarkerAtPlayhead(): Promise<void>;
  /** Flip `magnetic` on `trackId`, else the selected video track, else the first video track. */
  toggleMagnet(trackId?: string): Promise<void>;

  ensureStarterTracks(quiet?: boolean): Promise<void>;
  ensureTrack(kind: TrackKind, name: string): Promise<Track>;
  importMediaSmart(path: string): Promise<void>;
  placeMediaOnTimeline(mediaId: string, kind: MediaKind, quiet?: boolean): Promise<void>;

  startPlayback(): Promise<void>;
  stopPlayback(): void;
  seekTo(us: number): Promise<void>;
  scrubAt(us: number): void;

  onPlaybackTick(payload: { time_us: number; playing: boolean }): void;
  onPlaybackState(payload: { playing: boolean; time_us: number }): void;
  onPlaybackPerf(payload: ipc.PlaybackPerfPayload): void;
  togglePerfHud(): void;
  bumpThemeEpoch(): void;
}

export const useEditorStore = create<EditorStore>((set, get) => ({
  project: null,
  projectPath: null,
  clientRevision: 0,
  playhead: 0,
  playing: false,
  activeSequenceId: null,
  sequencePath: [],
  selection: null,
  selections: [],
  toolMode: "select",
  snapEnabled: true,
  pxPerSec: 80,
  scrollX: 0,
  scrollY: 0,
  leftTab: "media",
  canUndo: false,
  canRedo: false,
  toasts: [],
  importBusy: false,
  clipboard: null,
  dragGhost: null,
  transitionDropTarget: null,
  contextMenu: null,
  markerMenu: null,
  snapGuideUs: null,
  isDragging: false,
  previewOverrides: {},
  mediaAssets: {},
  perfSample: null,
  perfHudVisible: false,
  themeEpoch: 0,
  saveStatus: null,

  toast(message, kind = "info") {
    const id = nextToastId++;
    set((s) => ({ toasts: [...s.toasts, { id, message, kind }] }));
    window.setTimeout(() => get().dismissToast(id), 3200);
  },
  dismissToast(id) {
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
  },

  select(sel) {
    const selections = sel ? [sel] : [];
    set({ selection: sel, selections });
    void ipc.setEditorSelection(sel ? { primary: sel, all: selections } : null);
  },
  toggleSelect(sel) {
    const { selections, project } = get();
    const tracks = activeTracks(project, get().activeSequenceId);
    const idx = selections.findIndex((s) => s.trackId === sel.trackId && s.clipId === sel.clipId);
    let next: Selection[];
    if (idx >= 0) {
      next = selections.filter((_, i) => i !== idx);
    } else if (selections.length > 0 && project) {
      // Prefer same kind across tracks; different kind replaces the selection.
      const primaryTrack = tracks.find((t) => t.id === selections[0]!.trackId);
      const newTrack = tracks.find((t) => t.id === sel.trackId);
      if (primaryTrack && newTrack && primaryTrack.kind !== newTrack.kind) {
        next = [sel];
      } else {
        next = [...selections, sel];
      }
    } else {
      next = [...selections, sel];
    }
    const primary = next[0] ?? null;
    set({ selection: primary, selections: next });
    void ipc.setEditorSelection(primary ? { primary, all: next } : null);
  },
  setSelections(sels) {
    const primary = sels[0] ?? null;
    set({ selection: primary, selections: sels });
    void ipc.setEditorSelection(primary ? { primary, all: sels } : null);
  },
  async openSequence(sequenceId, opts) {
    const { project, sequencePath } = get();
    if (!project || !project.sequences?.some((s) => s.id === sequenceId)) return;
    await switchContainer(set, get, opts?.fromRoot ? [sequenceId] : pathAfterOpen(sequencePath, sequenceId));
  },
  async closeSequence() {
    const { sequencePath } = get();
    if (sequencePath.length === 0) return;
    await switchContainer(set, get, sequencePath.slice(0, -1));
  },
  async navigateToSequence(sequenceId) {
    await switchContainer(set, get, pathForBreadcrumb(get().sequencePath, sequenceId));
  },
  setTool(tool) {
    set({ toolMode: tool });
  },
  setSnap(enabled) {
    set({ snapEnabled: enabled });
  },
  setZoom(px) {
    set({ pxPerSec: Math.min(400, Math.max(20, px)) });
    // Clamp scroll after zoom so we don't leave empty space.
    const { project, scrollX, scrollY, pxPerSec } = get();
    if (!project) return;
    const canvas = document.getElementById("timeline");
    if (!canvas) return;
    get().setScroll(
      Math.min(scrollX, maxScrollX(containerView(project, get().activeSequenceId), pxPerSec, canvas.clientWidth)),
      Math.min(scrollY, maxScrollY(activeTracks(project, get().activeSequenceId).length, canvas.clientHeight)),
    );
  },
  fitZoom() {
    const project = get().project;
    if (!project) return;
    const canvas = document.getElementById("timeline");
    if (!canvas) return;
    const availableWidth = canvas.clientWidth - TRACK_LABEL_W - 16;
    const duration = timelineDurationUs(containerView(project, get().activeSequenceId));
    if (duration <= 0 || availableWidth <= 0) return;
    get().setZoom(availableWidth / usToSecs(duration));
    get().setScroll(0, get().scrollY);
  },
  setScroll(scrollX, scrollY) {
    const project = get().project;
    const canvas = document.getElementById("timeline");
    const w = canvas?.clientWidth ?? 800;
    const h = canvas?.clientHeight ?? 200;
    const view = project ? containerView(project, get().activeSequenceId) : null;
    const maxX = view ? maxScrollX(view, get().pxPerSec, w) : 0;
    const maxY = view ? maxScrollY(view.tracks.length, h) : 0;
    set({
      scrollX: Math.max(0, Math.min(maxX, scrollX)),
      scrollY: Math.max(0, Math.min(maxY, scrollY ?? get().scrollY)),
    });
  },
  panBy(dx, dy) {
    get().setScroll(get().scrollX + dx, get().scrollY + dy);
  },
  ensurePlayheadVisible() {
    const { project, playhead, pxPerSec, scrollX } = get();
    if (!project) return;
    const canvas = document.getElementById("timeline");
    if (!canvas) return;
    const margin = 64;
    const screenX = clipLeft(playhead, pxPerSec, scrollX);
    if (screenX < TRACK_LABEL_W + margin) {
      get().setScroll(scrollX - (TRACK_LABEL_W + margin - screenX));
    } else if (screenX > canvas.clientWidth - margin) {
      get().setScroll(scrollX + (screenX - (canvas.clientWidth - margin)));
    }
  },
  setLeftTab(tab) {
    set({ leftTab: tab });
  },
  setPlayheadLocal(us) {
    set({ playhead: Math.max(0, Math.round(us)) });
  },
  setDragging(dragging) {
    set({ isDragging: dragging });
  },
  onThumbnailsReady(payload) {
    const meta: Omit<ThumbnailAsset, "image"> = {
      stripUrl: ipc.assetUrl(payload.strip_path),
      cols: payload.cols,
      rows: payload.rows,
      tileWidth: payload.tile_width,
      tileHeight: payload.tile_height,
      intervalUs: payload.interval_us,
    };
    const image = new Image();
    image.onload = () => {
      set((s) => ({
        mediaAssets: {
          ...s.mediaAssets,
          [payload.media_id]: { ...s.mediaAssets[payload.media_id], thumbnails: { ...meta, image } },
        },
      }));
    };
    image.src = meta.stripUrl;
    set((s) => ({
      mediaAssets: {
        ...s.mediaAssets,
        [payload.media_id]: {
          ...s.mediaAssets[payload.media_id],
          thumbnails: { ...meta, image: null },
        },
      },
    }));
  },
  onWaveformReady(payload) {
    set((s) => ({
      mediaAssets: {
        ...s.mediaAssets,
        [payload.media_id]: {
          ...s.mediaAssets[payload.media_id],
          waveform: { peaks: payload.peaks, bucketUs: payload.bucket_us },
        },
      },
    }));
  },
  setDragGhost(ghost) {
    set({ dragGhost: ghost });
  },

  setTransitionDropTarget(target) {
    set({ transitionDropTarget: target });
  },
  openContextMenu(x, y, trackId, clipId, atUs) {
    const sel = { trackId, clipId };
    const { selections } = get();
    const already =
      selections.length > 1 &&
      selections.some((s) => s.trackId === trackId && s.clipId === clipId);
    if (already) {
      // Keep multi-selection; promote the right-clicked clip to primary.
      const rest = selections.filter((s) => !(s.trackId === trackId && s.clipId === clipId));
      const next = [sel, ...rest];
      set({ contextMenu: { x, y, trackId, clipId, atUs }, selection: sel, selections: next });
      void ipc.setEditorSelection({ primary: sel, all: next });
    } else {
      set({
        contextMenu: { x, y, trackId, clipId, atUs },
        selection: sel,
        selections: [sel],
      });
      void ipc.setEditorSelection({ primary: sel, all: [sel] });
    }
  },
  closeContextMenu() {
    set({ contextMenu: null });
  },
  openMarkerMenu(x, y, markerId) {
    set({ markerMenu: { x, y, markerId }, contextMenu: null });
  },
  closeMarkerMenu() {
    set({ markerMenu: null });
  },
  setSnapGuide(us) {
    set({ snapGuideUs: us });
  },

  async dropMediaOnTimeline() {
    const { project, dragGhost, activeSequenceId } = get();
    set({ dragGhost: null });
    if (!project || !dragGhost || !dragGhost.valid) return;
    const tracks = activeTracks(project, activeSequenceId);

    if (dragGhost.durationUs <= 0) {
      get().toast("That sequence is empty - add clips inside it first", "error");
      return;
    }
    // A nested sequence may land on video or audio lanes; for an auto-created track, dropping
    // above every lane makes a video track and below makes an audio track.
    const trackKind: TrackKind = newTrackKindForDrop(dragGhost.kind, dragGhost.row < 0);
    const existingTrack =
      dragGhost.trackIndex != null ? (tracks[dragGhost.trackIndex] as Track | undefined) : undefined;

    if (existingTrack) {
      await get().dispatch(
        addClip(existingTrack.id, dragGhost.mediaId, dragGhost.startUs, 0, dragGhost.durationUs),
      );
      return;
    }

    // Dropped above the topmost overlay lane or below the bottommost lane — CapCut-style
    // auto-track: AddTrack + AddClip as one atomic batch, so undo removes both together
    // instead of leaving an empty track. `AddTrack` always appends to the END of
    // `project.tracks`, which (per `layout.displayOrder`'s documented mapping) lands a new
    // video/image track at the visual TOP of the overlay group and a new audio track at
    // the visual BOTTOM of the audio group — i.e. always right where the user dropped it,
    // regardless of which edge triggered the auto-create. The new track's id is generated
    // here (not server-side) so the AddClip in the SAME batch can reference it —
    // apply_commands has no way to thread one command's server-generated output into a
    // later command in the same call.
    const trackName =
      trackKind === "video" ? `Video ${tracks.length + 1}` : `Audio ${tracks.length + 1}`;
    const newTrackId = crypto.randomUUID();
    await get().dispatchBatch([
      addTrack(trackKind, trackName, newTrackId, activeSequenceId),
      addClip(newTrackId, dragGhost.mediaId, dragGhost.startUs, 0, dragGhost.durationUs),
    ]);
  },

  async refetchProject() {
    if (!get().projectPath) return;
    try {
      const snap = await ipc.getProject();
      set({ project: snap.project, clientRevision: snap.revision });
    } catch (e) {
      console.warn("refetch project:", e);
    }
  },

  async loadProjectFromPath(path) {
    try {
      await ipc.openProject(path);
      const snap = await ipc.getProject();
      set({
        project: snap.project,
        clientRevision: snap.revision,
        projectPath: path,
        playhead: 0,
        activeSequenceId: null,
        sequencePath: [],
        selection: null,
        selections: [],
        canUndo: false,
        canRedo: false,
        previewOverrides: {},
        mediaAssets: {},
      });
      void ipc.setEditorSelection(null);
      void ipc.setActiveSequence(null).catch(() => {});
    } catch (e) {
      get().toast(`Load project failed: ${errMsg(e)}`, "error");
    }
  },

  async quickStart() {
    if (get().project) return true;
    set({ importBusy: true });
    try {
      const path = await ipc.quickStartProject();
      await get().loadProjectFromPath(path);
      await get().ensureStarterTracks(true);
      // G3: land on the media bin so the import-forward empty state is front and center.
      set({ leftTab: "media", saveStatus: "saved" });
      return true;
    } catch (e) {
      get().toast(`Could not start project: ${errMsg(e)}`, "error");
      return false;
    } finally {
      set({ importBusy: false });
    }
  },

  async createNewProject(path, name) {
    await ipc.newProject(path, name);
    await get().loadProjectFromPath(path);
    await get().ensureStarterTracks(true);
    set({ leftTab: "media", saveStatus: "saved" });
  },

  async saveProject() {
    if (!get().projectPath) return;
    set({ saveStatus: "saving" });
    try {
      await ipc.saveProject();
      set({ saveStatus: "saved" });
    } catch (e) {
      set({ saveStatus: null });
      get().toast(`Save failed: ${errMsg(e)}`, "error");
    }
  },

  async closeProject() {
    try {
      get().stopPlayback();
      await ipc.closeProject();
      void ipc.setActiveSequence(null).catch(() => {});
      set({
        project: null,
        projectPath: null,
        clientRevision: 0,
        playhead: 0,
        playing: false,
        activeSequenceId: null,
        sequencePath: [],
        selection: null,
        selections: [],
        canUndo: false,
        canRedo: false,
        previewOverrides: {},
        mediaAssets: {},
        saveStatus: null,
        clipboard: null,
      });
    } catch (e) {
      get().toast(`Close failed: ${errMsg(e)}`, "error");
    }
  },

  setPreviewOverride(clipId, over) {
    set((s) => ({ previewOverrides: { ...s.previewOverrides, [clipId]: over } }));
    void ipc.setPreviewOverride(clipId, over).catch(() => {});
  },

  clearPreviewOverride(clipId) {
    if (!(clipId in get().previewOverrides)) return;
    set((s) => {
      const next = { ...s.previewOverrides };
      delete next[clipId];
      return { previewOverrides: next };
    });
    void ipc.clearPreviewOverride(clipId).catch(() => {});
  },

  previewEffectOnSelection(entry) {
    const { project, selections, activeSequenceId } = get();
    if (!project) return;
    const tracks = activeTracks(project, activeSequenceId);
    const targets: string[] = [];
    for (const sel of selections) {
      const clip = tracks.find((t) => t.id === sel.trackId)?.clips.find((c) => c.id === sel.clipId);
      if (!clip || clip.type === "caption") continue;
      get().setPreviewOverride(clip.id, {
        ...get().previewOverrides[clip.id],
        effects: hoverStack(clip, entry, crypto.randomUUID()),
      });
      targets.push(clip.id);
    }
    ensurePreviewPackItems([entry.ref]);
    for (const id of effectPreviewTargets) {
      if (!targets.includes(id)) get().clearPreviewOverride(id);
    }
    effectPreviewTargets = targets;
  },

  previewTransitionOnSelection(entry, durationUs) {
    const { project, selections, activeSequenceId } = get();
    if (!project) return;
    const tracks = activeTracks(project, activeSequenceId);
    const targets: string[] = [];
    for (const sel of selections) {
      const track = tracks.find((t) => t.id === sel.trackId);
      const clip = track?.clips.find((c) => c.id === sel.clipId);
      if (!clip || (clip.type !== "video" && clip.type !== "text")) continue;
      get().setPreviewOverride(clip.id, {
        transition: {
          ref: entry.ref,
          duration_us: Math.round(durationUs),
          params: initialParams(entry),
          easing: null,
        },
      });
      targets.push(clip.id);
    }
    ensurePreviewPackItems([entry.ref]);
    for (const id of effectPreviewTargets) {
      if (!targets.includes(id)) get().clearPreviewOverride(id);
    }
    effectPreviewTargets = targets;
  },

  clearEffectPreview() {
    for (const id of effectPreviewTargets) get().clearPreviewOverride(id);
    effectPreviewTargets = [];
  },

  endGesture() {
    void ipc.endGesture().catch(() => {});
  },

  async dispatch(command, quiet = false, opts = {}) {
    if (!get().project) {
      get().toast("Create or open a project first.", "error");
      return false;
    }
    // No unconditional pause/seek here (that was BX1: every inspector tweak or drag
    // commit forced a pause + reseek + audio restart mid-playback). `refreshFrame`
    // live-swaps the edited project into the running play session when playing, or
    // renders one paused-state frame otherwise — see its doc comment in `lib/ipc.ts`.
    const mutationId = crypto.randomUUID();
    selfMutationIds.add(mutationId);
    try {
      const result = await ipc.applyCommand(command, mutationId, opts.mergeKey);
      if (!applyProjectPatch(get, set, result.revision, result.patch)) {
        await get().refetchProject();
      }
      await ipc.refreshFrame(get().playhead).catch(() => {});
      return true;
    } catch (e) {
      if (!quiet) get().toast(formatCommandError(e), "error");
      // The backend rejected the command, so `store.project` may still be holding an
      // optimistic local mutation (e.g. a timeline drag preview) that was never actually
      // applied — resync from the real backend state instead of leaving that phantom
      // edit visible until some unrelated later command happens to refetch.
      await get().refetchProject();
      return false;
    } finally {
      selfMutationIds.delete(mutationId);
    }
  },

  async dispatchBatch(commands, quiet = false, opts = {}) {
    if (!get().project) {
      get().toast("Create or open a project first.", "error");
      return false;
    }
    const mutationId = crypto.randomUUID();
    selfMutationIds.add(mutationId);
    try {
      const result = await ipc.applyCommands(commands, mutationId, opts.mergeKey);
      if (!applyProjectPatch(get, set, result.revision, result.patch)) {
        await get().refetchProject();
      }
      await ipc.refreshFrame(get().playhead).catch(() => {});
      return true;
    } catch (e) {
      if (!quiet) get().toast(formatCommandError(e), "error");
      await get().refetchProject();
      return false;
    } finally {
      selfMutationIds.delete(mutationId);
    }
  },

  async undo() {
    const mutationId = crypto.randomUUID();
    selfMutationIds.add(mutationId);
    try {
      const status = await ipc.undo(mutationId);
      set({ canUndo: status.can_undo, canRedo: status.can_redo });
      if (status.patch.length > 0) {
        if (!applyProjectPatch(get, set, status.revision, status.patch)) {
          await get().refetchProject();
        }
      } else {
        set({ clientRevision: status.revision });
      }
      get().pruneStaleSelection();
      await ipc.refreshFrame(get().playhead).catch(() => {});
    } catch (e) {
      get().toast(formatCommandError(e, "Undo failed"), "error");
    } finally {
      selfMutationIds.delete(mutationId);
    }
  },

  async redo() {
    const mutationId = crypto.randomUUID();
    selfMutationIds.add(mutationId);
    try {
      const status = await ipc.redo(mutationId);
      set({ canUndo: status.can_undo, canRedo: status.can_redo });
      if (status.patch.length > 0) {
        if (!applyProjectPatch(get, set, status.revision, status.patch)) {
          await get().refetchProject();
        }
      } else {
        set({ clientRevision: status.revision });
      }
      get().pruneStaleSelection();
      await ipc.refreshFrame(get().playhead).catch(() => {});
    } catch (e) {
      get().toast(formatCommandError(e, "Redo failed"), "error");
    } finally {
      selfMutationIds.delete(mutationId);
    }
  },

  pruneStaleSelection() {
    // Undo/redo can restore a project where selected clips no longer exist.
    const { project, selections } = get();
    if (!project) return;
    if (selections.length === 0) return;
    const tracks = activeTracks(project, get().activeSequenceId);
    const next = selections.filter((sel) => {
      const track = tracks.find((t) => t.id === sel.trackId);
      return !!track?.clips.find((c) => c.id === sel.clipId);
    });
    if (next.length === selections.length) return;
    const primary = next[0] ?? null;
    set({ selection: primary, selections: next });
    void ipc.setEditorSelection(primary ? { primary, all: next } : null);
  },

  copySelection() {
    const { project, selections } = get();
    if (!project || selections.length === 0) return;
    const items: { clip: Clip; trackKind: TrackKind }[] = [];
    const tracks = activeTracks(project, get().activeSequenceId);
    for (const sel of selections) {
      const track = tracks.find((t) => t.id === sel.trackId);
      const clip = track?.clips.find((c) => c.id === sel.clipId);
      if (track && clip) items.push({ clip, trackKind: track.kind });
    }
    if (items.length === 0) return;
    set({ clipboard: { items } });
    get().toast(items.length === 1 ? "Copied" : `Copied ${items.length} clips`, "info");
  },

  async pasteAtPlayhead() {
    const { project, clipboard, playhead } = get();
    if (!project || !clipboard || clipboard.items.length === 0) return;
    const anchor = Math.min(...clipboard.items.map((i) => i.clip.start_us));
    const commands: Record<string, unknown>[] = [];
    const tracks = activeTracks(project, get().activeSequenceId);
    for (const item of clipboard.items) {
      const track =
        (get().selection &&
        tracks.find((t) => t.id === get().selection!.trackId)?.kind === item.trackKind
          ? tracks.find((t) => t.id === get().selection!.trackId)
          : undefined) ?? tracks.find((t) => t.kind === item.trackKind);
      if (!track) {
        get().toast(`No ${item.trackKind} track to paste onto`, "error");
        return;
      }
      const pos = playhead + (item.clip.start_us - anchor);
      commands.push(pasteCommand(item.clip, track.id, frameSnapped(project, Math.max(0, pos))));
    }
    if (commands.length === 1) await get().dispatch(commands[0]!);
    else await get().dispatchBatch(commands);
  },

  async duplicateSelection() {
    const { project, selections } = get();
    if (!project || selections.length === 0) return;
    const commands: Record<string, unknown>[] = [];
    const tracks = activeTracks(project, get().activeSequenceId);
    for (const sel of selections) {
      const track = tracks.find((t) => t.id === sel.trackId);
      const clip = track?.clips.find((c) => c.id === sel.clipId);
      if (!track || !clip) continue;
      commands.push(pasteCommand(clip, track.id, clip.start_us + clipDurationUs(clip)));
    }
    if (commands.length === 0) return;
    if (commands.length === 1) await get().dispatch(commands[0]!);
    else await get().dispatchBatch(commands);
  },

  async nestSelection() {
    const { project, selections, activeSequenceId } = get();
    if (!project || selections.length === 0) return;
    const tracks = activeTracks(project, activeSequenceId);
    for (const sel of selections) {
      if (tracks.find((t) => t.id === sel.trackId)?.locked) {
        get().toast("Track is locked — unlock it to edit", "info");
        return;
      }
    }
    const name = `Nested ${(project.sequences?.length ?? 0) + 1}`;
    const ok = await get().dispatch(nestClips(selections, name));
    if (ok) {
      get().select(null);
      get().toast(`Nested ${selections.length === 1 ? "1 clip" : `${selections.length} clips`} into “${name}”`, "success");
    }
  },

  async unnestClip(trackId, clipId) {
    const sel = get().selection;
    const t = trackId ?? sel?.trackId;
    const c = clipId ?? sel?.clipId;
    if (!t || !c) return;
    // The engine refuses trimmed / sped-up / effected clips (CannotUnnest) — dispatch's
    // error toast carries its reason.
    const ok = await get().dispatch(unnestClip(t, c));
    if (ok) {
      get().select(null);
      get().toast("Unnested — the sequence is kept in the media panel", "success");
    }
  },

  async addAdjustmentLayerAtPlayhead(trackId, durationUs = 5_000_000) {
    const { project, activeSequenceId, selection, playhead } = get();
    if (!project) return;
    const tracks = activeTracks(project, activeSequenceId);
    const videoTracks = tracks.filter((t) => t.kind === "video");
    const target =
      (trackId ? tracks.find((t) => t.id === trackId) : undefined) ??
      (selection ? videoTracks.find((t) => t.id === selection.trackId) : undefined) ??
      // Highest video track in the compositing order (last = top).
      videoTracks[videoTracks.length - 1];
    if (target && target.kind !== "video") {
      get().toast("Adjustment layers go on video tracks", "error");
      return;
    }
    if (target) {
      const before = new Set(target.clips.map((c) => c.id));
      const ok = await get().dispatch(addAdjustmentLayer(target.id, frameSnapped(project, playhead), durationUs));
      if (!ok) return;
      const created = activeTracks(get().project, activeSequenceId)
        .find((t) => t.id === target.id)
        ?.clips.find((c) => !before.has(c.id));
      if (created) get().select({ trackId: target.id, clipId: created.id });
      return;
    }
    // No video track in this container yet: create it and the layer atomically.
    const newTrackId = crypto.randomUUID();
    const ok = await get().dispatchBatch([
      addTrack("video", `Video ${tracks.length + 1}`, newTrackId, activeSequenceId),
      addAdjustmentLayer(newTrackId, frameSnapped(project, playhead), durationUs),
    ]);
    if (!ok) return;
    const created = activeTracks(get().project, activeSequenceId).find((t) => t.id === newTrackId)?.clips[0];
    if (created) get().select({ trackId: newTrackId, clipId: created.id });
  },

  async addTitleAtPlayhead(text = "Title", opts = {}) {
    const { project, activeSequenceId, selection } = get();
    if (!project) return;
    const durationUs = opts.durationUs ?? DEFAULT_TITLE_US;
    const style = titleStyleFor(opts.styleRef ?? DEFAULT_TEXT_STYLE_REF);
    const start = frameSnapped(project, get().playhead);
    const tracks = activeTracks(project, activeSequenceId);
    const free = (t: Track) =>
      !t.locked &&
      !t.magnetic &&
      !t.clips.some((c) => start < c.start_us + clipDurationUs(c) && c.start_us < start + durationUs);
    const videoTracks = tracks.filter((t) => t.kind === "video");
    // Selected video track first, then from the topmost lane down.
    const ordered = [
      ...videoTracks.filter((t) => t.id === selection?.trackId),
      ...[...videoTracks].reverse().filter((t) => t.id !== selection?.trackId),
    ];
    const target = ordered.find(free);
    let created: { trackId: string; before: Set<string> };
    if (target) {
      created = { trackId: target.id, before: new Set(target.clips.map((c) => c.id)) };
      if (!(await get().dispatch(addText(target.id, text, start, durationUs, style)))) return;
    } else {
      const newTrackId = crypto.randomUUID();
      const commands: Record<string, unknown>[] = [];
      // No video track yet: the first one is the (magnetic) main track, titles go above it.
      if (videoTracks.length === 0) {
        commands.push(addTrack("video", "Video 1", null, activeSequenceId));
      }
      commands.push(
        addTrack("video", "Titles", newTrackId, activeSequenceId, false),
        addText(newTrackId, text, start, durationUs, style),
      );
      created = { trackId: newTrackId, before: new Set() };
      if (!(await get().dispatchBatch(commands))) return;
    }
    const clip = activeTracks(get().project, activeSequenceId)
      .find((t) => t.id === created.trackId)
      ?.clips.find((c) => !created.before.has(c.id));
    if (clip) get().select({ trackId: created.trackId, clipId: clip.id });
  },

  async addMarkerAtPlayhead() {
    const { project, activeSequenceId, playhead } = get();
    if (!project) return;
    if (activeSequenceId) {
      get().toast("Markers belong to the main timeline - leave the nested sequence first", "info");
      return;
    }
    const at = frameSnapped(project, playhead);
    if ((project.markers ?? []).some((m) => m.time_us === at)) {
      get().toast("There is already a marker here", "info");
      return;
    }
    await get().dispatch(addMarkers([{ timeUs: at, label: "" }]));
  },

  async toggleMagnet(trackId) {
    const { project, activeSequenceId, selection } = get();
    if (!project) return;
    const tracks = activeTracks(project, activeSequenceId);
    const track = trackId
      ? tracks.find((t) => t.id === trackId)
      : magnetTargetTrack(tracks, selection?.trackId);
    if (!track || track.kind === "caption") {
      get().toast("Add a video track to use the magnet", "info");
      return;
    }
    await get().dispatch(setTrackFlags(track.id, { magnetic: !track.magnetic }));
  },

  async ensureStarterTracks(quiet = false) {
    // Deliberately the ROOT timeline (a fresh project's starter tracks), not the open container.
    const project = get().project;
    if (!project || project.tracks.length > 0) return;
    await get().dispatch({ command: "AddTrack", kind: "video", name: "Video 1" }, true);
    await get().dispatch({ command: "AddTrack", kind: "audio", name: "Audio 1" }, true);
    await get().dispatch({ command: "AddTrack", kind: "caption", name: "Captions" }, true);
    if (!quiet) get().toast("Timeline ready — import media to start editing", "info");
  },

  async ensureTrack(kind, name) {
    const activeId = get().activeSequenceId;
    const existing = activeTracks(get().project, activeId).find((t) => t.kind === kind);
    if (existing) return existing;
    const ok = await get().dispatch(
      activeId ? addTrack(kind, name, null, activeId) : { command: "AddTrack", kind, name },
      true,
    );
    if (!ok) throw new Error(`Could not create ${kind} track`);
    const track = activeTracks(get().project, activeId).find((t) => t.kind === kind);
    if (!track) throw new Error(`Track ${kind} missing after creation`);
    return track;
  },

  async importMediaSmart(path) {
    if (!get().project) {
      get().toast("Create or open a project first.", "error");
      return;
    }
    set({ importBusy: true });
    try {
      const ok = await get().dispatch({ command: "ImportMedia", path });
      if (!ok) return;
      const mediaList = get().project?.media ?? [];
      const media = mediaList[mediaList.length - 1];
      if (!media) return;
      await get().placeMediaOnTimeline(media.id, media.kind, true);
      get().toast(`Added ${basename(path)} to timeline`, "success");
    } finally {
      set({ importBusy: false });
    }
  },

  async placeMediaOnTimeline(mediaId, kind, quiet = false) {
    const project = get().project;
    if (!project) return;
    const media = project.media.find((m) => m.id === mediaId);
    if (!media) return;
    const trackKind: TrackKind = kind === "audio" ? "audio" : "video";
    const dur = mediaDurationUs(project, media) ?? 5_000_000;
    if (media.kind === "sequence" && dur <= 0) {
      if (!quiet) get().toast("That sequence is empty - add clips inside it first", "error");
      return;
    }
    const track = await get().ensureTrack(trackKind, trackKind === "video" ? "Video 1" : "Audio 1");

    // Placing at the playhead is usually right, but if that would land on top of an
    // existing clip on this track (e.g. clicking an already-placed media item again),
    // fall back to appending after the last clip instead of failing with an overlap
    // error — CapCut-style "just make it fit" rather than a dead-end command failure.
    const desired = frameSnapped(project, get().playhead);
    const overlaps = track.clips.some(
      (c) => desired < c.start_us + clipDurationUs(c) && c.start_us < desired + dur,
    );
    const position = overlaps
      ? track.clips.reduce((end, c) => Math.max(end, c.start_us + clipDurationUs(c)), 0)
      : desired;

    const ok = await get().dispatch({
      command: "AddClip",
      track_id: track.id,
      media_id: mediaId,
      start: position,
      source_in: 0,
      source_out: dur,
    });
    if (ok && !quiet) {
      const label = sequenceMediaName(project, media) ?? basename(media.path);
      get().toast(`Added ${label} to timeline`, "success");
    }
  },

  async startPlayback() {
    if (!get().project) return;
    set({ playing: true });
    try {
      await ipc.play(get().playhead);
    } catch (e) {
      set({ playing: false });
      get().toast(formatCommandError(e, "Playback failed"), "error");
    }
  },

  stopPlayback() {
    set({ playing: false });
    ipc
      .pause()
      .then((timeUs) => set({ playhead: timeUs }))
      .catch(() => {});
  },

  async seekTo(us) {
    const clamped = Math.max(0, Math.round(us));
    set({ playhead: clamped });
    try {
      await ipc.seek(clamped);
    } catch (e) {
      get().toast(formatCommandError(e, "Seek failed"), "error");
    }
  },

  scrubAt(us) {
    const clamped = Math.max(0, Math.round(us));
    set({ playhead: clamped });
    void ipc.scrubAudio(clamped).catch(() => {});
  },

  onPlaybackTick(payload) {
    set({ playhead: payload.time_us });
    get().ensurePlayheadVisible();
  },
  onPlaybackState(payload) {
    set({
      playing: payload.playing,
      playhead: payload.time_us,
      // Perf samples are only meaningful mid-playback — clear on stop so the HUD doesn't
      // keep showing a frozen last-second-of-playback reading forever while paused.
      perfSample: payload.playing ? get().perfSample : null,
    });
  },
  onPlaybackPerf(payload) {
    set({ perfSample: payload });
  },
  togglePerfHud() {
    set((s) => ({ perfHudVisible: !s.perfHudVisible }));
  },
  bumpThemeEpoch() {
    set((s) => ({ themeEpoch: s.themeEpoch + 1 }));
  },
}));

// Dev-only devtools handle (also used by the browser harness to drive state). No-op in
// production builds' consumers; harmless if present.
if ((import.meta as { env?: { DEV?: boolean } }).env?.DEV) {
  (globalThis as unknown as { __store?: typeof useEditorStore }).__store = useEditorStore;
}

/// Move the editor to the container at the end of `path` (a normalized sequence path; `[]` =
/// root). Playback is stopped, selection cleared and the playhead rewound — playhead, scroll
/// and selection are all local to a container. Backend playback/scrub audio and the preview
/// follow via `ipc.setActiveSequence` / the preview engine's subscription to this state.
/// History is global, so undo/redo keep working across containers.
async function switchContainer(
  set: (partial: Partial<EditorStore>) => void,
  get: () => EditorStore,
  path: string[],
): Promise<void> {
  const next = normalizeSequencePath(get().project, path);
  const nextId = next[next.length - 1] ?? null;
  if (nextId === get().activeSequenceId) return;
  if (get().playing) get().stopPlayback();
  set({
    sequencePath: next,
    activeSequenceId: nextId,
    selection: null,
    selections: [],
    playhead: 0,
    scrollX: 0,
    scrollY: 0,
    contextMenu: null,
    dragGhost: null,
  });
  void ipc.setEditorSelection(null);
  try {
    await ipc.setActiveSequence(nextId);
  } catch (e) {
    console.warn("set_active_sequence failed:", e);
  }
  await ipc.seek(0).catch(() => {});
}

// Fallback to the root: when the project changes (undo, delete, external edit) and the open
// sequence no longer exists, drop to the nearest surviving ancestor. Done in one place so no
// individual code path (patch apply, refetch, undo/redo, bridge) can leave the editor pointing
// at a container that is gone.
useEditorStore.subscribe((state, prev) => {
  if (state.project === prev.project || state.sequencePath.length === 0) return;
  const valid = normalizeSequencePath(state.project, state.sequencePath);
  if (valid.length === state.sequencePath.length) return;
  const nextId = valid[valid.length - 1] ?? null;
  useEditorStore.setState({
    sequencePath: valid,
    activeSequenceId: nextId,
    selection: null,
    selections: [],
    playhead: 0,
    scrollX: 0,
    scrollY: 0,
  });
  void ipc.setActiveSequence(nextId).catch(() => {});
  void ipc.setEditorSelection(null);
});

function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

/// Surfaces the backend `CommandError` / Tauri error string without a redundant
/// "Edit failed:" prefix when the message is already self-describing.
export function formatCommandError(e: unknown, fallbackPrefix = "Edit failed"): string {
  const msg = errMsg(e).trim();
  if (!msg) return fallbackPrefix;
  // The engine reports the range in integer microseconds.
  const overlap = /clip range \[(-?\d+), (-?\d+)\) overlaps an existing clip on track [0-9a-f-]{36}/i.exec(msg);
  if (overlap) {
    const secs = (v: string) => `${(Number(v) / 1_000_000).toFixed(1)}s`;
    return `Can't place it there: ${secs(overlap[1])}–${secs(overlap[2])} overlaps another clip on that track`;
  }
  // Engine errors that carry a bare UUID read badly in a toast; say what to do instead.
  if (/sequence [0-9a-f-]{36} is still placed/i.test(msg)) {
    return "That sequence is still used by a clip - delete or unnest those clips first";
  }
  // Tauri often wraps as `commandname failed: …` or plain Display from thiserror.
  if (/failed|error|reject|invalid|not found|overlap|mismatch|cannot/i.test(msg)) return msg;
  return `${fallbackPrefix}: ${msg}`;
}

function basename(path: string): string {
  return path.split(/[/\\]/).pop() ?? path;
}

/// Nearest frame boundary of the project rate (µs).
function frameSnapped(project: Project, us: number): number {
  return snapToFrameUs(normalizeFps(project.settings.fps), us);
}

function pasteCommand(clip: Clip, trackId: string, startUs: number): Record<string, unknown> {
  if (clip.type === "caption") {
    return addCaption(trackId, bodyArg(clip.body), startUs, clip.duration_us, clip.style, clip.animations);
  }
  if (clip.type === "text") {
    return addText(trackId, bodyArg(clip.body), startUs, clip.duration_us, clip.style, clip.animations);
  }
  if (clip.type === "adjustment") {
    return addAdjustmentLayer(trackId, startUs, clip.duration_us);
  }
  return addClip(trackId, clip.media_id, startUs, clip.source_in_us, clip.source_out_us);
}

/// Wires backend events (playback + project mutation) into the store. Call once at app
/// startup. Returns an unsubscribe function.
export function connectStoreToBackendEvents(): () => void {
  const unsubTick = ipc.onPlaybackTick((p) => useEditorStore.getState().onPlaybackTick(p));
  const unsubState = ipc.onPlaybackState((p) => useEditorStore.getState().onPlaybackState(p));
  const unsubPlayErr = ipc.onPlaybackError((p) => {
    useEditorStore.getState().toast(p.message, "error");
  });
  const unsubChanged = ipc.onProjectChanged((p) => {
    useEditorStore.setState({
      canUndo: p.can_undo,
      canRedo: p.can_redo,
      // G2: every successful edit already hit disk via backend `commit_project`.
      saveStatus: "saved",
    });
    // Skip the refetch while a timeline drag is in progress — see `isDragging`'s doc
    // comment. The undo/redo flags above are harmless to update regardless.
    if (useEditorStore.getState().isDragging) return;
    // B2: skip for edits this tab already applied via the mutation response patch.
    if (p.mutation_id && selfMutationIds.has(p.mutation_id)) return;
    // B3: already at this revision (e.g. duplicate event) — nothing to do.
    if (p.revision === useEditorStore.getState().clientRevision) return;
    // External edit (or revision gap) — events don't carry the patch, so full refetch.
    void useEditorStore.getState().refetchProject();
  });
  const unsubThumbs = ipc.onThumbnailsReady((p) => useEditorStore.getState().onThumbnailsReady(p));
  const unsubWaveform = ipc.onWaveformReady((p) => useEditorStore.getState().onWaveformReady(p));
  const unsubPerf = ipc.onPlaybackPerf((p) => useEditorStore.getState().onPlaybackPerf(p));
  const unsubBridgePlayhead = ipc.onBridgePlayhead((p) => {
    void useEditorStore.getState().seekTo(p.time_us);
  });
  return () => {
    unsubTick();
    unsubState();
    unsubPlayErr();
    unsubChanged();
    unsubThumbs();
    unsubWaveform();
    unsubPerf();
    unsubBridgePlayhead();
  };
}
