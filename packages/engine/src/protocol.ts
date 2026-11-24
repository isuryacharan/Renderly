// Messages between the main thread and the render worker. Times are integer flicks.

import type { Flicks, MediaInput, OpenResult } from "@renderly/media";
import type { PreviewScale, Size } from "./layout";
import type { Cut } from "./timeline";

export interface LayerStats {
  layer: number;
  /** Distinct frames uploaded per second over the last report interval. */
  fps: number;
  presented: number;
  dropped: number;
  late: number;
  /** Keyframe-only updates, used when the layer had no decoder. */
  keyframes: number;
  /** True when the layer's current run has a decoder stream. */
  decoding: boolean;
}

export interface RenderStats {
  /** The lowest per-layer fps among layers showing something, over the last report interval. */
  fps: number;
  /** Totals since the last play started. */
  presented: number;
  /** Frames decoded but never shown: the playhead had passed them, or the context was lost. */
  dropped: number;
  /** Display refreshes where the frame due was not decoded yet (the picture held too long). */
  late: number;
  /** CPU time of the worker's per-refresh work, in ms. */
  tickMeanMs: number;
  tickMaxMs: number;
  /** Longest gap between two display refreshes in the worker. */
  refreshMaxMs: number;
  playhead: Flicks;
  /** Time from the play request to the clock starting, for the last play. */
  startLatencyMs: number;
  playing: boolean;
  contextLost: boolean;
  contextRestores: number;
  working: Size;
  /** Frames on loan from the decoder right now (the leak counter). */
  framesOutstanding: number;
  /** Frames received and waiting for their time. */
  queued: number;
  /** True when playback follows the audio clock. */
  audioClock: boolean;
  /** Start time of the first frame the current play put on screen. */
  firstShown: Flicks | null;
  /**
   * Picture against sound, checked every refresh while following the audio clock: how far the
   * heard time is into the frame on screen, in ms. In sync means 0 <= offset < frame duration.
   */
  sync: { checks: number; outOfSync: number; minMs: number; maxMs: number };
  /** Cut boundaries crossed since the last play started. */
  cuts: {
    passed: number;
    /** Boundaries where the old picture stayed up for at least one refresh. */
    held: number;
    maxHoldMs: number;
    /** Boundaries whose first frame was not the source frame due at that moment. */
    inexact: number;
    /** Frames decoded for either side of a cut but never shown. */
    droppedAtCuts: number;
  };
  decoders: {
    pool: number;
    /** Decoders decoding a stream right now (the pool keeps one more for keyframes and seeks). */
    busy: number;
    /** File opens since the last play started. */
    opens: number;
  };
  layers: LayerStats[];
  /**
   * GPU memory the renderer accounts for: render targets, the upload texture, decoded frames on
   * loan. Peak since the last play started. `refused`: targets not created at the ceiling.
   */
  memory: { usedMB: number; peakMB: number; ceilingMB: number; refused: number };
}

export interface LoadResult {
  /** One result per source, in order. */
  sources: OpenResult[];
  /** The edit list in use: the one given, or one cut over the whole of source 0. */
  cuts: Cut[];
  duration: Flicks;
}

export type ToRenderer =
  | {
      type: "init";
      canvas: OffscreenCanvas;
      /** One port per decode worker in the pool. */
      decoders: MessagePort[];
      /** The audio clock (see @renderly/audio ClockBuffer). */
      audioClock: SharedArrayBuffer;
      size: Size;
    }
  /** The canvas's size in device pixels changed. */
  | { type: "resize"; size: Size }
  /** Opens the sources and sets the edit list (null: one cut over the whole of source 0). */
  | { type: "load"; id: number; sources: MediaInput[]; cuts: Cut[] | null }
  /** `session` is the audio session to follow, or 0 to run on the display refresh. */
  | { type: "play"; from: Flicks; session: number }
  /** Stops and replies with the start of the frame on screen, where playback should resume. */
  | { type: "pause"; id: number }
  /** Shows the exact frame at `time`. */
  | { type: "seek"; id: number; time: Flicks }
  | { type: "preview-scale"; scale: PreviewScale }
  /** Test hook: loses the WebGL context and restores it after `restoreAfterMs`. */
  | { type: "lose-context"; restoreAfterMs: number };

export type FromRenderer =
  | { type: "ready" }
  | { type: "loaded"; id: number; result: LoadResult }
  | { type: "seeked"; id: number; time: Flicks | null }
  | { type: "paused"; id: number; time: Flicks }
  | { type: "stats"; stats: RenderStats }
  /** `id` is set when the error belongs to a request (open, seek); that request fails. */
  | { type: "error"; id?: number; message: string };
