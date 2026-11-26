// @renderly/core: the project model. No DOM, React or GPU: it runs in workers and plain tests.

export * from "./time";
export { createId, deriveId, ID_LENGTH, ID_PATTERN, type RandomBytes } from "./ids";
export {
  componentDefaults,
  componentSchema,
  componentTypes,
  Ease,
  Keyframe,
  Keys,
  registerComponent,
} from "./components";
export {
  CURRENT_VERSION,
  formatPath,
  Marker,
  Media,
  PackRef,
  projectSchema,
  type Clip,
  type MediaItem,
  type Project,
  type Sequence,
  type Track,
} from "./schema";
export {
  migrate,
  parseProject,
  serialiseProject,
  type MigrateResult,
  type Problem,
} from "./migrate";
export { diffEntities, type Changes } from "./changes";
export {
  CommandError,
  commandTypes,
  defineCommand,
  failure,
  getCommand,
  type Command,
  type CommandDefinition,
  type Failure,
  type FailureCode,
} from "./commands";
export {
  ProjectStore,
  type DispatchOptions,
  type DispatchResult,
  type Gesture,
  type HistoryEntry,
  type StoreOptions,
  type UndoResult,
} from "./store";
export {
  AddMarker,
  MoveMarker,
  RemoveMarker,
  ResetComponent,
  SetComponentField,
  SetComponentFieldMany,
  SetProjectName,
} from "./basic-commands";
export { INITIAL_VIEW, ViewStore, type ViewState } from "./view-state";
export {
  checkTimeline,
  clipAt,
  clipEnd,
  clipsInRange,
  sequenceDuration,
  snapTargets,
  snapTo,
  sourceDuration,
  sourceSpan,
  type ClipRef,
} from "./timeline";
export {
  AddClips,
  AddTrack,
  CloseGaps,
  DeleteClips,
  MoveClips,
  RemoveTracks,
  RippleDeleteClips,
  SetTrackFlags,
  SplitClips,
  deleteClips,
  TrimClips,
} from "./timeline-commands";
export { newProject, type NewProjectOptions } from "./new-project";
export { ImportMedia, RemoveMedia } from "./media-commands";
export {
  cutPoints,
  evaluateSequence,
  frameAt,
  graphHash,
  IDENTITY_TRANSFORM,
  stableHash,
  type EvaluateOptions,
  type FrameGraph,
  type FrameLayer,
  type GraphClip,
  type GraphLayer,
  type LayerTransform,
  type SequenceGraph,
} from "./evaluate";
