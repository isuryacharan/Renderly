// The ONLY file that imports @tauri-apps/api or @tauri-apps/plugin-dialog. Every backend
// call and event subscription is typed and funneled through here — components never call
// `invoke`/`listen` directly (enforced by grep gate, see docs/architecture.md).

import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { Project } from "./types";

export interface HistoryStatus {
  can_undo: boolean;
  can_redo: boolean;
}

export interface ProjectChangedPayload {
  revision: number;
  can_undo: boolean;
  can_redo: boolean;
  /// Echoes the `mutationId` passed into `applyCommand`/`applyCommands`/`undo`/`redo`, if
  /// any — lets the listener skip a redundant refetch for edits it already fetched itself.
  mutation_id?: string | null;
}

export interface PlaybackTickPayload {
  time_secs: number;
  playing: boolean;
}

export interface PlaybackStatePayload {
  playing: boolean;
  time_secs: number;
}

export interface PlaybackErrorPayload {
  message: string;
}

/// ~1Hz averaged perf sample from the playback loop (improvement-plan A0 perf HUD).
export interface PlaybackPerfPayload {
  decode_ms: number;
  compose_ms: number;
  present_ms: number;
  frame_ms: number;
  fps: number;
}

export interface DragDropPayload {
  paths?: string[];
}

export interface ThumbnailsReadyPayload {
  media_id: string;
  strip_path: string;
  cols: number;
  rows: number;
  tile_width: number;
  tile_height: number;
  interval_secs: number;
}

export interface WaveformReadyPayload {
  media_id: string;
  peaks: number[];
  bucket_secs: number;
}

export type ExportPhase = "video" | "audio" | "mux";

export interface ExportProgressPayload {
  phase: ExportPhase;
  frame: number;
  total_frames: number;
  fraction: number;
}

/** Shorthand presets or a full `ExportPreset` JSON value (e.g. custom w/h/fps). */
export type ExportPresetArg =
  | "tiktok"
  | "youtube"
  | { custom: { width: number; height: number; fps: number } };

export interface MediaAssetsPayload {
  thumbnails?: ThumbnailsReadyPayload;
  waveform?: WaveformReadyPayload;
}

export interface PackStickerInfo {
  id: string;
  label: string;
  default_duration_secs: number;
}

export interface PackSfxInfo {
  id: string;
  label: string;
}

export interface PackLutInfo {
  id: string;
  label: string;
}

export interface PackTransitionInfo {
  id: string;
  label: string;
  kind: string;
  default_duration_secs: number;
}

export interface PackTemplateInfo {
  id: string;
  label: string;
}

export interface LoadedPackInfo {
  id: string;
  name: string;
  path: string;
  stickers: PackStickerInfo[];
  sfx: PackSfxInfo[];
  luts: PackLutInfo[];
  transitions: PackTransitionInfo[];
  templates: PackTemplateInfo[];
}

export interface LoadedPluginInfo {
  id: string;
  name: string;
  path: string;
  has_frame: boolean;
  has_audio: boolean;
}

export interface ExtensionCatalog {
  packs: LoadedPackInfo[];
  plugins: LoadedPluginInfo[];
}

export interface RegistryEntry {
  id: string;
  kind: "pack" | "plugin";
  path?: string;
  git_url?: string;
  summary: string;
  schema_version: number;
  resolved_path?: string | null;
}

// ---- Project lifecycle ----

export function quickStartProject(): Promise<string> {
  return invoke<string>("quick_start_project");
}

export function newProject(path: string, name: string): Promise<void> {
  return invoke("new_project", { path, name });
}

export function openProject(path: string): Promise<void> {
  return invoke("open_project", { path });
}

export function saveProject(): Promise<void> {
  return invoke("save_project");
}

export function getProject(): Promise<ProjectSnapshot> {
  return invoke<ProjectSnapshot>("get_project");
}

export interface ProjectSummary {
  path: string;
  name: string;
  durationSecs: number;
  modifiedMs: number;
  width: number;
  height: number;
  fps: number;
  thumbPath?: string | null;
}

export function listProjects(): Promise<ProjectSummary[]> {
  return invoke<ProjectSummary[]>("list_projects");
}

export function renameProject(path: string, newName: string): Promise<void> {
  return invoke("rename_project", { path, newName });
}

export function deleteProject(path: string): Promise<void> {
  return invoke("delete_project", { path });
}

export function projectThumbnail(path: string): Promise<string | null> {
  return invoke<string | null>("project_thumbnail", { path });
}

export function closeProject(): Promise<void> {
  return invoke("close_project");
}

// ---- Commands / history ----

/** RFC-6902 operation (matches `json-patch` / `fast-json-patch` wire shape). */
export type JsonPatchOp = {
  op: string;
  path: string;
  from?: string;
  value?: unknown;
};

/** App-IPC result of `apply_command` (B3) — not part of the core Command enum. */
export interface CommandResult {
  revision: number;
  patch: JsonPatchOp[];
  outcome: string;
}

export interface CommandsResult {
  revision: number;
  patch: JsonPatchOp[];
  outcomes: string[];
}

export interface HistoryResult {
  can_undo: boolean;
  can_redo: boolean;
  revision: number;
  patch: JsonPatchOp[];
}

export interface ProjectSnapshot {
  project: Project;
  revision: number;
}

/// `mutationId`, if given, comes back on the resulting `project:changed` event so the
/// caller can recognize "this is my own edit" and skip a second refetch — see B2 in
/// docs/improvement-plan.md and the `project:changed` handler in `store/editorStore.ts`.
export function applyCommand(
  command: Record<string, unknown>,
  mutationId?: string,
): Promise<CommandResult> {
  return invoke<CommandResult>("apply_command", { command, mutationId });
}

/// Atomic batch — all-or-nothing, one undo step. For gestures that are logically a
/// single edit but need more than one Command (e.g. auto-track-on-drop).
export function applyCommands(
  commands: Record<string, unknown>[],
  mutationId?: string,
): Promise<CommandsResult> {
  return invoke<CommandsResult>("apply_commands", { commands, mutationId });
}

export function undo(mutationId?: string): Promise<HistoryResult> {
  return invoke<HistoryResult>("undo", { mutationId });
}

export function redo(mutationId?: string): Promise<HistoryResult> {
  return invoke<HistoryResult>("redo", { mutationId });
}

// ---- Export ----

export function exportProject(
  outputPath: string,
  preset: ExportPresetArg | Record<string, unknown>,
  encode?: { videoEncoder?: string; crf?: number; audioBitrateK?: number },
): Promise<void> {
  return invoke("export_project", { outputPath, preset, encode });
}

export function cancelExport(): Promise<void> {
  return invoke("cancel_export");
}

// ---- Media assets (thumbnails / waveforms) ----

export function requestMediaAssets(mediaId: string): Promise<void> {
  return invoke("request_media_assets", { mediaId });
}

export function getMediaAssets(mediaId: string): Promise<MediaAssetsPayload> {
  return invoke<MediaAssetsPayload>("get_media_assets", { mediaId });
}

/// Converts a real filesystem path (as returned by `get_media_assets`/`media:*-ready`)
/// into a URL the webview can load — backed by the Tauri asset protocol, scoped to the
/// media cache dir (see `tauri.conf.json`'s `security.assetProtocol.scope`).
export function assetUrl(path: string): string {
  return convertFileSrc(path);
}

// ---- Preview compositor host assets (raster/generated mattes, pack .cube LUTs) ----

/// Backend for `preview/previewAssets.ts`. The commands only serve files the open project
/// references (src-tauri/src/preview_assets.rs), so no asset-protocol scope is involved.
export const previewAssetBackend = {
  readMatte(path: string): Promise<ArrayBuffer> {
    return invoke<ArrayBuffer>("read_preview_matte", { path });
  },
  matteStamps(paths: string[]): Promise<number[]> {
    return invoke<number[]>("preview_matte_stamps", { paths });
  },
  readPackLuts(
    wanted: { pack_id: string; lut_id: string }[],
  ): Promise<{ pack_id: string; lut_id: string; cube: string }[]> {
    return invoke("read_preview_pack_luts", { wanted });
  },
};

// ---- Preview / playback ----

export function play(timeSecs: number): Promise<void> {
  return invoke("play", { timeSecs });
}

export function pause(): Promise<number> {
  return invoke<number>("pause");
}

/** Mirror timeline selection into the Rust backend for bridge `get_editor_status`. */
export function setEditorSelection(
  selection: {
    primary: { trackId: string; clipId: string } | null;
    all: { trackId: string; clipId: string }[];
  } | null,
): Promise<void> {
  return invoke("set_editor_selection", { selection });
}

export function seek(timeSecs: number): Promise<void> {
  return invoke("seek", { timeSecs });
}

/// Refresh playback after an edit (apply_command/apply_commands/undo/redo) without the
/// disruption of a real seek. While playing, this live-swaps the project into the running
/// play session (no pause, no audio restart) so the next audio chunk reflects the edit;
/// the preview canvas repaints itself from the same store patch regardless of play state
/// — see the Rust `refresh_frame` command's doc comment. `timeSecs` is accepted for call-
/// site symmetry with `seek` but isn't forwarded (the backend command no longer needs it).
export function refreshFrame(timeSecs: number): Promise<void> {
  void timeSecs;
  return invoke("refresh_frame");
}

export function scrubAudio(timeSecs: number): Promise<void> {
  return invoke("scrub_audio", { timeSecs });
}

// ---- Events ----

export function onPlaybackTick(cb: (payload: PlaybackTickPayload) => void): () => void {
  const unlisten = listen<PlaybackTickPayload>("playback:tick", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

export function onPlaybackState(cb: (payload: PlaybackStatePayload) => void): () => void {
  const unlisten = listen<PlaybackStatePayload>("playback:state", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

export function onPlaybackError(cb: (payload: PlaybackErrorPayload) => void): () => void {
  const unlisten = listen<PlaybackErrorPayload>("playback:error", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

export function onPlaybackPerf(cb: (payload: PlaybackPerfPayload) => void): () => void {
  const unlisten = listen<PlaybackPerfPayload>("playback:perf", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

/** E3: agent `set_playhead` over the live bridge — sync GUI playhead. */
export function onBridgePlayhead(cb: (payload: { time_secs: number }) => void): () => void {
  const unlisten = listen<{ time_secs: number }>("bridge:playhead", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

export function onProjectChanged(cb: (payload: ProjectChangedPayload) => void): () => void {
  const unlisten = listen<ProjectChangedPayload>("project:changed", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

export function onThumbnailsReady(cb: (payload: ThumbnailsReadyPayload) => void): () => void {
  const unlisten = listen<ThumbnailsReadyPayload>("media:thumbnails-ready", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

export function onWaveformReady(cb: (payload: WaveformReadyPayload) => void): () => void {
  const unlisten = listen<WaveformReadyPayload>("media:waveform-ready", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

export function onExportProgress(cb: (payload: ExportProgressPayload) => void): () => void {
  const unlisten = listen<ExportProgressPayload>("export:progress", (e) => cb(e.payload));
  return () => void unlisten.then((f) => f());
}

export function onDragDrop(cb: (paths: string[]) => void): () => void {
  const unlisten = listen<DragDropPayload>("tauri://drag-drop", (e) =>
    cb(e.payload.paths ?? []),
  );
  return () => void unlisten.then((f) => f());
}

export function onDragEnter(cb: () => void): () => void {
  const unlisten = listen("tauri://drag-enter", () => cb());
  return () => void unlisten.then((f) => f());
}

export function onDragLeave(cb: () => void): () => void {
  const unlisten = listen("tauri://drag-leave", () => cb());
  return () => void unlisten.then((f) => f());
}

// ---- Native dialogs ----

export const MEDIA_OPEN_FILTERS = [
  { name: "Video", extensions: ["mp4", "mov", "mkv", "webm", "avi"] },
  { name: "Images", extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp"] },
  { name: "Audio", extensions: ["mp3", "wav", "m4a", "aac", "flac", "ogg"] },
  {
    name: "All media",
    extensions: [
      "mp4",
      "mov",
      "mkv",
      "webm",
      "avi",
      "png",
      "jpg",
      "jpeg",
      "webp",
      "gif",
      "bmp",
      "mp3",
      "wav",
      "m4a",
      "aac",
      "flac",
      "ogg",
    ],
  },
];

function firstPath(result: string | string[] | null): string | null {
  if (!result) return null;
  return Array.isArray(result) ? (result[0] ?? null) : result;
}

export async function pickMediaFile(): Promise<string | null> {
  const result = await open({
    multiple: false,
    title: "Choose video, image, or audio",
    filters: MEDIA_OPEN_FILTERS,
  });
  return firstPath(result);
}

export async function pickProjectFileToOpen(): Promise<string | null> {
  const result = await open({
    filters: [{ name: "Renderly project", extensions: ["renderly.json"] }],
  });
  return firstPath(result);
}

export async function pickProjectSavePath(): Promise<string | null> {
  const result = await save({
    filters: [{ name: "Renderly project", extensions: ["renderly.json"] }],
  });
  return firstPath(result);
}

export async function pickExportSavePath(defaultPath: string): Promise<string | null> {
  const result = await save({
    filters: [{ name: "MP4 video", extensions: ["mp4"] }],
    defaultPath,
  });
  return firstPath(result);
}

export async function pickExtensionFolder(title: string): Promise<string | null> {
  const result = await open({
    directory: true,
    multiple: false,
    title,
  });
  return firstPath(result);
}

export function listExtensions(): Promise<ExtensionCatalog> {
  return invoke<ExtensionCatalog>("list_extensions");
}

export function listRegistry(): Promise<RegistryEntry[]> {
  return invoke<RegistryEntry[]>("list_registry");
}

// ---- Window chrome (frameless titlebar) ----

export function minimizeWindow(): Promise<void> {
  return getCurrentWindow().minimize();
}

export function toggleMaximizeWindow(): Promise<void> {
  return getCurrentWindow().toggleMaximize();
}

export function closeWindow(): Promise<void> {
  return getCurrentWindow().close();
}

export function isWindowMaximized(): Promise<boolean> {
  return getCurrentWindow().isMaximized();
}

