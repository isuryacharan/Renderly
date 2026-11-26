// Keeps the engine showing what the project says. Every project, media or view change is
// evaluated (core `evaluateSequence`) and, when what plays changed (`graphHash`), sent to the
// engine as an in-place update: one at a time, always the newest. The playhead comes back from
// the render worker through shared memory and is copied into the view store once per display
// frame while playing, so nothing goes through React.
//
// A playhead the user moves turns into a seek (a click), a scrub (a drag), or a restart (while
// playing). `peek` shows another time without moving the playhead (the edge frame while trimming).

import {
  cutPoints,
  evaluateSequence,
  flicksPerFrame,
  graphHash,
  type ProjectStore,
  type SequenceGraph,
  type ViewStore,
} from "@renderly/core";
import { cutsFromGraph, type Preview, type RenderStats, type ViewerView } from "@renderly/engine";
import type { MediaStatus } from "../editor/media-status";

/** Refreshes to wait for a play request to take effect before giving up (about 2 s at 60 Hz). */
const START_TIMEOUT_REFRESHES = 120;

export class EngineLink {
  private readonly sources = new Map<File, number>();
  private sourceCount = 0;
  private sentHash: string | null = null;
  private syncing = false;
  private again = false;
  private graph: SequenceGraph | null = null;
  /** The playhead as the engine last reported it, so its echo in the view store is ignored. */
  private engineTime: number | null = null;
  private seekChain: Promise<unknown> = Promise.resolve();
  private seekWanted: number | null = null;
  private frameId = 0;
  /** The engine's first (empty) load is done: updates can be sent. */
  private loaded = false;
  /** The renderer has reported playing since the last play request. */
  private started = false;
  /** The renderer's refresh count when the last play was requested. */
  private startRefresh = 0;
  /** A peek was showing: when it ends, the viewer goes back to the playhead. */
  private peeking = false;
  private readonly cleanup: (() => void)[] = [];
  private closed = false;
  /** Updates sent to the engine, and how long the last one took (for tests). */
  updates = 0;
  lastUpdateMs = 0;
  /**
   * For each update while playing: display refreshes between the edit and the engine having
   * applied it (its next refresh shows it). 1 means the next frame on screen has the edit.
   */
  readonly updateRefreshes: number[] = [];
  /** Engine errors since the link started, and its latest stats report. */
  readonly errors: string[] = [];
  stats: RenderStats | null = null;

  /** Shows an error and keeps it in `errors` (tests read it). */
  private report(message: string): void {
    this.errors.push(message);
    this.onError(message);
  }

  constructor(
    readonly preview: Preview,
    private readonly store: ProjectStore,
    private readonly view: ViewStore,
    private readonly status: MediaStatus,
    private readonly onError: (message: string) => void,
  ) {
    preview.onError((message) => this.errors.push(message));
    preview.onStats((stats) => (this.stats = stats));
    this.cleanup.push(store.subscribe(() => this.sync()));
    this.cleanup.push(status.subscribe(() => this.sync()));
    this.cleanup.push(
      view.select(
        (s) => s.playhead,
        (t) => this.playheadMoved(t),
      ),
    );
    this.cleanup.push(
      view.select(
        (s) => s.scrubbing,
        (on) => (on ? this.startScrub() : this.endScrub()),
      ),
    );
    this.cleanup.push(
      view.select(
        (s) => s.peek,
        (t) => (t === null ? this.endScrub() : this.scrubTo(t)),
      ),
    );
    this.cleanup.push(
      view.select(
        (s) => s.sequence,
        () => this.sync(),
      ),
    );
    const loop = () => {
      this.frameId = requestAnimationFrame(loop);
      this.follow();
    };
    this.frameId = requestAnimationFrame(loop);
  }

  async start(): Promise<void> {
    const sequence = this.sequence();
    if (!sequence) return;
    await this.preview.load([], [], { width: sequence.width, height: sequence.height });
    // Edits made while the engine was loading were not sent (the load would have wiped them):
    // send the project as it is now.
    this.loaded = true;
    await this.sync();
  }

  dispose(): void {
    this.closed = true;
    cancelAnimationFrame(this.frameId);
    for (const fn of this.cleanup) fn();
    this.preview.dispose();
  }

  setView(view: ViewerView, background: number): void {
    this.preview.setView(view, background);
  }

  private sequence() {
    const project = this.store.project;
    const id = this.view.get().sequence;
    return project.sequences.find((s) => s.id === id) ?? project.sequences[0];
  }

  // --- project to engine ---

  /** Sends the newest edit list if it changed. Coalesces: one update in flight at a time. */
  async sync(): Promise<void> {
    if (this.closed || !this.loaded) return;
    if (this.syncing) {
      this.again = true;
      return;
    }
    this.syncing = true;
    try {
      do {
        this.again = false;
        await this.syncOnce();
      } while (this.again && !this.closed);
    } catch (error) {
      this.report(`Preview: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      this.syncing = false;
    }
  }

  private async syncOnce(): Promise<void> {
    const sequence = this.sequence();
    if (!sequence) return;
    const fileOf = (media: string) => {
      const state = this.status.get(media);
      return state?.status === "online" && !state.unsupported ? state.file : null;
    };
    const graph = evaluateSequence(this.store.project, sequence.id, {
      available: (media) => fileOf(media) !== null,
    });
    this.graph = graph;
    // Hash the files too: a relinked item is a different file under the same media id.
    const files = graph.layers.flatMap((l) => l.clips.map((c) => fileOf(c.media)));
    const hash = `${graphHash(graph)}:${files.map((f) => (f ? this.indexOf(f, false) : -1)).join(",")}`;
    if (hash === this.sentHash) return;
    const added: File[] = [];
    const cuts = cutsFromGraph(graph, (media) => {
      const file = fileOf(media);
      if (!file) return null;
      let source = this.indexOf(file, false);
      if (source < 0) {
        source = this.indexOf(file, true);
        added.push(file);
      }
      return source;
    });
    const started = performance.now();
    const before = this.preview.readPlayhead();
    const loaded = await this.preview.update(added, cuts, {
      width: graph.width,
      height: graph.height,
    });
    // Applied before the refresh after `before`: the next frame on screen has the edit (1).
    if (before.playing && this.preview.readPlayhead().playing && loaded.refresh !== undefined)
      this.updateRefreshes.push(Math.max(0, loaded.refresh - before.refreshes) + 1);
    this.lastUpdateMs = performance.now() - started;
    this.updates++;
    this.sentHash = hash;
  }

  private indexOf(file: File, add: boolean): number {
    const known = this.sources.get(file);
    if (known !== undefined) return known;
    if (!add) return -1;
    const index = this.sourceCount++;
    this.sources.set(file, index);
    return index;
  }

  // --- transport ---

  get duration(): number {
    return this.graph?.duration ?? 0;
  }

  private frame(): number {
    const sequence = this.sequence();
    return sequence ? flicksPerFrame(sequence.fps) : 1;
  }

  /** The span playback loops over: the selected clips, or the whole sequence. */
  loopRange(): { from: number; to: number } {
    const selected = new Set(this.view.get().selection);
    let from = Infinity;
    let to = -Infinity;
    for (const track of this.sequence()?.tracks ?? [])
      for (const clip of track.clips)
        if (selected.has(clip.id)) {
          from = Math.min(from, clip.start);
          to = Math.max(to, clip.start + clip.duration);
        }
    return from < to ? { from, to } : { from: 0, to: this.duration };
  }

  play(): void {
    // The renderer plays drawing layers; a timeline with nothing to draw does not start.
    if (!this.graph?.layers.some((layer) => layer.clips.length > 0)) return;
    const { loop, playhead } = this.view.get();
    let from = playhead;
    if (loop) {
      const range = this.loopRange();
      if (from < range.from || from >= range.to) from = range.from;
    } else if (from >= this.duration) from = 0;
    this.startAt(from);
  }

  private startAt(from: number): void {
    // A seek still waiting to be sent would arrive after the play request and stop playback (seen
    // on slow machines, where exact seeks queue up behind each other): drop it.
    this.seekWanted = null;
    this.started = false;
    this.startRefresh = this.preview.readPlayhead().refreshes;
    this.view.set({ playing: true, peek: null, scrubbing: false });
    this.echo(from);
    this.preview.play(from);
  }

  async pause(): Promise<void> {
    if (!this.view.get().playing) return;
    this.view.set({ playing: false });
    const time = await this.preview.pause();
    // The frame on screen starts at its source timestamp, which need not sit exactly on the
    // sequence's frame grid (container timestamps are rounded): rest on the grid.
    const frame = this.frame();
    this.echo(Math.round(time / frame) * frame);
  }

  toggle(): void {
    if (this.view.get().playing) void this.pause();
    else this.play();
  }

  /** Moves the playhead by `frames` (negative is earlier), pausing first. */
  async step(frames: number): Promise<void> {
    await this.pause();
    const frame = this.frame();
    const t = Math.round(this.view.get().playhead / frame) * frame + frames * frame;
    this.view.set({ playhead: Math.max(0, t) });
  }

  /** Jumps to the previous or next cut on any drawing layer. */
  async jump(direction: -1 | 1): Promise<void> {
    await this.pause();
    if (!this.graph) return;
    const points = [...cutPoints(this.graph), this.duration];
    const t = this.view.get().playhead;
    const target =
      direction > 0 ? points.find((p) => p > t) : [...points].reverse().find((p) => p < t);
    if (target !== undefined) this.view.set({ playhead: target });
  }

  /** Copies the engine's playhead into the view store while playing; handles the end and loops. */
  private follow(): void {
    if (!this.view.get().playing) return;
    const { time, playing } = this.preview.readPlayhead();
    if (playing) this.started = true;
    if (!this.started) {
      // The play request has not taken effect yet. If it never does (about 2 s of refreshes),
      // stop waiting so the transport does not stay in "playing".
      if (this.preview.readPlayhead().refreshes - this.startRefresh > START_TIMEOUT_REFRESHES) {
        this.view.set({ playing: false });
        this.report("Playback did not start.");
      }
      return;
    }
    if (this.view.get().loop) {
      const range = this.loopRange();
      if (time >= range.to || !playing) {
        this.startAt(range.from);
        return;
      }
    }
    if (!playing) {
      // Stopped by itself: the end of the sequence.
      this.view.set({ playing: false });
      this.echo(Math.min(time, this.duration));
      return;
    }
    if (time !== this.engineTime) this.echo(time);
  }

  private echo(t: number): void {
    this.engineTime = t;
    this.view.set({ playhead: t });
  }

  private playheadMoved(t: number): void {
    if (t === this.engineTime) return;
    this.engineTime = null;
    const { playing, scrubbing } = this.view.get();
    if (playing) this.startAt(t);
    else if (scrubbing) this.preview.scrub(t);
    else this.seek(t);
  }

  /** Shows the exact frame at `t`; only the newest of several quick requests runs. */
  private seek(t: number): void {
    const queued = this.seekWanted !== null;
    this.seekWanted = t;
    if (queued) return;
    this.seekChain = this.seekChain.then(async () => {
      const wanted = this.seekWanted;
      this.seekWanted = null;
      if (wanted === null || this.closed) return;
      try {
        await this.preview.seek(wanted);
      } catch (error) {
        this.report(`Preview: ${error instanceof Error ? error.message : String(error)}`);
      }
    });
  }

  private startScrub(): void {
    this.seekWanted = null;
    if (this.view.get().playing) void this.pause();
    this.preview.scrub(this.view.get().playhead);
  }

  private scrubTo(t: number): void {
    this.seekWanted = null;
    if (this.view.get().playing) void this.pause();
    this.peeking = true;
    this.preview.scrub(t);
  }

  private endScrub(): void {
    const { scrubbing, peek } = this.view.get();
    if (scrubbing || peek !== null) return;
    const peeked = this.peeking;
    this.peeking = false;
    void this.preview.scrubEnd().then(() => {
      if (peeked && !this.view.get().playing) this.seek(this.view.get().playhead);
    });
  }
}
