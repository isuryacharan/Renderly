// Messages between a decode worker and its client. All times are integer flicks relative to the
// first video frame of the file (source time 0 = first frame).

import type { Flicks } from "./time";

/**
 * Where the bytes come from. A `File` (from a picker or a stored handle) is the product path.
 * `url` exists for automated benchmarks, which cannot use a file picker; it is read by HTTP range
 * requests, so it is still never loaded whole.
 */
export type MediaInput = Blob | { url: string };

export type Rotation = 0 | 90 | 180 | 270;

export type SeekMode = "exact" | "keyframe";

export interface TrackInfo {
  kind: "video" | "audio";
  codec: string | null;
}

export interface MediaInfo {
  /** From the first video frame to the end of the last one. */
  duration: Flicks;
  width: number;
  height: number;
  rotation: Rotation;
  /** The video codec, e.g. "avc" or "hevc". */
  codec: string;
  tracks: TrackInfo[];
}

export type OpenFailure =
  /** The container could not be read at all. */
  | { reason: "unsupported-format"; detail: string }
  | { reason: "no-video-track" }
  /** The browser has no decoder for this codec or profile. */
  | { reason: "unsupported-codec"; codec: string | null };

export type OpenResult = ({ ok: true } & MediaInfo) | ({ ok: false } & OpenFailure);

export interface DecodeStats {
  /** Frames sent to the client and not yet returned as credit. */
  framesInFlight: number;
  framesSent: number;
  /** Decoded frames held inside the worker. Zero whenever the worker is idle. */
  framesHeld: number;
  /** Exact-seek work: non-reference frames skipped and frames decoded. */
  seekFramesSkipped: number;
  seekFramesDecoded: number;
}

export type ToWorker =
  | { type: "open"; id: number; input: MediaInput; maxFramesInFlight: number }
  /** `paceMs`: minimum gap between decode submissions (see range-decode.ts); 0 or absent: none. */
  | { type: "play"; id: number; from: Flicks; to: Flicks; paceMs?: number }
  | { type: "seek"; id: number; time: Flicks; mode: SeekMode }
  /** Ends the current play or seek. */
  | { type: "stop"; id: number }
  /** The client closed this many frames; the worker may send as many more. */
  | { type: "credit"; count: number }
  | { type: "stats"; id: number }
  /**
   * Tests only: every seek waits this long before it starts, as on a slow machine (software
   * decoding), so races that need slow seeks show up on a fast one. 0 turns it off.
   */
  | { type: "test-seek-delay"; ms: number }
  | { type: "close"; id: number };

export type FromWorker =
  | { type: "opened"; id: number; result: OpenResult }
  /** A frame for the play or seek with this id. The receiver must close it. */
  | { type: "frame"; id: number; frame: VideoFrame; time: Flicks; duration: Flicks }
  /** A play or seek has finished, was stopped, or was replaced by a newer one. */
  | { type: "done"; id: number; frames: number }
  | { type: "stats"; id: number; stats: DecodeStats }
  | { type: "closed"; id: number }
  | { type: "error"; id: number; message: string };
