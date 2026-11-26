// The canvas timeline: the ruler and the track area on one canvas, redrawn only when something
// changed and drawing only what is visible. Pointer, wheel, keyboard and drop handling turn into
// typed commands; drags run as store gestures, so a drag is one undo step and Escape cancels it.
//
// Track headers and the toolbar are React (see Timeline.tsx); this class owns the canvas.

import {
  clipEnd,
  createId,
  flicksPerFrame,
  sequenceDuration,
  snapTargets,
  type Clip,
  type Command,
  type Gesture,
  type MediaItem,
  type Project,
  type ProjectStore,
  type Sequence,
  type Track,
  type ViewStore,
} from "@renderly/core";
import { ICONS } from "@renderly/ui";
import { colors, fonts, radius, spacing, textStyles, type ThemeId } from "@renderly/ui/tokens";
import type { MediaStatus } from "../editor/media-status";
import { onThemeChange, resolvedTheme } from "../theme";
import { draggedMedia } from "./drag";
import {
  CLIP_INSET,
  clipsInBox,
  edgeScroll,
  firstVisible,
  hitClip,
  layoutRows,
  rowAt,
  rowsHeight,
  RULER_HEIGHT,
  rulerLabel,
  rulerSteps,
  SNAP_PX,
  snapNearest,
  snapSpan,
  timeToX,
  wheelIntent,
  xToTime,
  zoomAround,
  zoomLimits,
  zoomToFit,
  type Row,
  type View,
} from "./geometry";
import { thumbKey, Thumbnails, type ThumbRequest } from "./thumbnails";

export type Tool = "select" | "blade";

const SCROLLBAR = 10;
const NAME_ROW = 18;
const AUDIO_STRIP = 14;
const DRAG_THRESHOLD = 3;

type Drag =
  | { kind: "scrub" }
  | { kind: "scrollbar"; startX: number; startT: number }
  | {
      kind: "pending-move" | "move";
      startX: number;
      startY: number;
      startT: number;
      rows: Row[];
      startRow: Row;
      clips: string[];
      primary: Clip;
      base: Sequence;
      gesture: Gesture | null;
      payload: unknown;
      trackOffset: number;
    }
  | {
      kind: "trim";
      startT: number;
      edge: "start" | "end";
      clips: string[];
      primary: Clip;
      base: Sequence;
      gesture: Gesture;
      payload: unknown;
    }
  | {
      kind: "box";
      startX: number;
      startY: number;
      startT: number;
      startScrollY: number;
      add: readonly string[];
      moved: boolean;
    };

interface Ghost {
  media: MediaItem;
  row: Row | null;
  start: number;
  duration: number;
}

export interface TimelineOptions {
  store: ProjectStore;
  view: ViewStore;
  status: MediaStatus;
  readOnly: () => boolean;
  onError: (message: string) => void;
  /** Called after the tool, snapping or vertical scroll changed. */
  onChange: () => void;
}

export class TimelineView {
  readonly canvas = document.createElement("canvas");
  private readonly ctx: CanvasRenderingContext2D;
  private width = 0;
  private height = 0;
  private dpr = 1;
  private dirty = true;
  private frameId = 0;
  private theme: ThemeId = "dark";

  tool: Tool = "select";
  snapping = true;
  /** Vertical scroll of the track rows, in pixels. */
  scrollY = 0;
  /** Milliseconds the last redraw took. */
  lastDrawMs = 0;
  draws = 0;

  private drag: Drag | null = null;
  private pointer = { x: 0, y: 0, ctrl: false };
  private snapLine: number | null = null;
  private bladeAt: number | null = null;
  private ghost: Ghost | null = null;
  private box: { x0: number; y0: number; x1: number; y1: number } | null = null;
  private follow = true;
  private readonly thumbs: Thumbnails;
  private readonly textWidths = new Map<string, number>();
  private readonly cleanup: (() => void)[] = [];

  constructor(
    private readonly host: HTMLElement,
    private readonly options: TimelineOptions,
  ) {
    const ctx = this.canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("No 2D canvas");
    this.ctx = ctx;
    this.canvas.className = "timeline__canvas";
    this.canvas.tabIndex = 0;
    this.canvas.setAttribute("role", "application");
    this.canvas.setAttribute("aria-label", "Timeline tracks");
    this.canvas.setAttribute("aria-roledescription", "timeline");
    host.appendChild(this.canvas);
    this.theme = resolvedTheme();
    this.scrollY = options.view.get().scroll.y;

    this.thumbs = new Thumbnails(
      (media) => {
        const state = options.status.get(media);
        return state?.status === "online" && !state.unsupported ? state.file : null;
      },
      () => this.invalidate(),
    );

    const resize = new ResizeObserver(() => this.resize());
    resize.observe(host);
    this.cleanup.push(() => resize.disconnect());
    this.resize();

    this.cleanup.push(options.store.subscribe(() => this.invalidate()));
    this.cleanup.push(options.status.subscribe(() => this.invalidate()));
    this.cleanup.push(
      options.view.subscribe(() => {
        this.invalidate();
      }),
    );
    this.cleanup.push(
      options.view.select(
        (s) => s.playhead,
        (t) => {
          if (this.follow && !this.drag) this.followPlayhead(t);
        },
      ),
    );

    this.cleanup.push(onThemeChange(() => this.retheme()));

    const on = <K extends keyof HTMLElementEventMap>(
      type: K,
      listener: (event: HTMLElementEventMap[K]) => void,
      opts?: AddEventListenerOptions,
    ) => {
      this.canvas.addEventListener(type, listener, opts);
      this.cleanup.push(() => this.canvas.removeEventListener(type, listener));
    };
    on("pointerdown", (e) => this.onDown(e));
    on("pointermove", (e) => this.onMove(e));
    on("pointerup", (e) => this.onUp(e));
    on("pointercancel", () => this.cancelDrag());
    on("lostpointercapture", () => {
      if (this.drag) this.onUp(null);
    });
    on("pointerleave", () => {
      if (this.bladeAt !== null && !this.drag) {
        this.bladeAt = null;
        this.invalidate();
      }
    });
    on("wheel", (e) => this.onWheel(e), { passive: false });
    on("keydown", (e) => this.onKey(e));
    on("dragover", (e) => this.onDragOver(e));
    on("dragleave", () => {
      this.ghost = null;
      this.snapLine = null;
      this.invalidate();
    });
    on("drop", (e) => this.onDrop(e));

    const escape = (e: KeyboardEvent) => {
      if (e.key === "Escape" && this.drag) {
        e.preventDefault();
        e.stopPropagation();
        this.cancelDrag();
      }
    };
    addEventListener("keydown", escape, true);
    this.cleanup.push(() => removeEventListener("keydown", escape, true));

    const loop = () => {
      this.frameId = requestAnimationFrame(loop);
      this.autoScroll();
      if (!this.dirty) return;
      this.dirty = false;
      this.redraw();
    };
    this.frameId = requestAnimationFrame(loop);
  }

  destroy(): void {
    cancelAnimationFrame(this.frameId);
    this.cancelDrag();
    for (const fn of this.cleanup) fn();
    this.thumbs.close();
    this.canvas.remove();
  }

  invalidate(): void {
    this.dirty = true;
  }

  /** Draws now and returns how long it took. */
  redraw(): number {
    const t0 = performance.now();
    this.draw();
    this.lastDrawMs = performance.now() - t0;
    this.draws++;
    return this.lastDrawMs;
  }

  get thumbnailStats() {
    return { cached: this.thumbs.size, pending: this.thumbs.pending };
  }

  /** Where a clip is drawn, in page pixels (for tests and tooltips); null if it is not found. */
  clipBox(id: string): { x: number; y: number; width: number; height: number } | null {
    const rect = this.canvas.getBoundingClientRect();
    for (const row of this.rows) {
      const clip = row.track.clips.find((c) => c.id === id);
      if (!clip) continue;
      const x = timeToX(this.viewport, clip.start);
      return {
        x: rect.left + x,
        y: rect.top + RULER_HEIGHT + row.y - this.scrollY + CLIP_INSET,
        width: clip.duration / this.viewport.zoom,
        height: row.height - CLIP_INSET * 2,
      };
    }
    return null;
  }

  /** The page x of time `t`. */
  pageX(t: number): number {
    return this.canvas.getBoundingClientRect().left + timeToX(this.viewport, t);
  }

  // --------------------------------------------------------------- state

  private get project(): Project {
    return this.options.store.project;
  }

  get sequence(): Sequence | undefined {
    const id = this.options.view.get().sequence;
    return this.project.sequences.find((s) => s.id === id) ?? this.project.sequences[0];
  }

  private get viewport(): View {
    const { scroll, zoom } = this.options.view.get();
    return { t: scroll.t, zoom, y: this.scrollY };
  }

  get rows(): Row[] {
    const sequence = this.sequence;
    return sequence ? layoutRows(sequence) : [];
  }

  private get trackAreaHeight(): number {
    return this.height - RULER_HEIGHT - SCROLLBAR;
  }

  private setScroll(t: number, y = this.scrollY): void {
    const sequence = this.sequence;
    const end = sequence ? sequenceDuration(sequence) : 0;
    const { zoom, scroll } = this.options.view.get();
    const maxT = Math.max(0, end + (this.width * zoom) / 2 - this.width * zoom);
    const clampedT = Math.min(Math.max(0, t), Math.max(maxT, scroll.t, 0));
    const maxY = Math.max(0, rowsHeight(this.rows) + 40 - this.trackAreaHeight);
    const clampedY = Math.min(Math.max(0, y), maxY);
    const changedY = clampedY !== this.scrollY;
    this.scrollY = clampedY;
    this.options.view.set({ scroll: { t: clampedT, y: clampedY } });
    if (changedY) this.options.onChange();
    this.invalidate();
  }

  setZoom(zoom: number, anchorX = this.width / 2): void {
    const sequence = this.sequence;
    if (!sequence) return;
    const { min, max } = zoomLimits(sequence, sequenceDuration(sequence), this.width);
    const next = zoomAround(this.viewport, Math.min(Math.max(zoom, min), max), anchorX);
    this.options.view.set({ zoom: next.zoom, scroll: { t: next.t, y: this.scrollY } });
    this.invalidate();
  }

  zoomBy(factor: number, anchorX?: number): void {
    const { zoom, playhead } = this.options.view.get();
    const playheadX = timeToX(this.viewport, playhead);
    const anchor = anchorX ?? (playheadX >= 0 && playheadX <= this.width ? playheadX : undefined);
    this.setZoom(zoom * factor, anchor);
  }

  fit(): void {
    const sequence = this.sequence;
    if (!sequence) return;
    const zoom = zoomToFit(sequence, sequenceDuration(sequence), this.width);
    this.options.view.set({ zoom, scroll: { t: 0, y: this.scrollY } });
    this.invalidate();
  }

  setTool(tool: Tool): void {
    this.tool = tool;
    this.bladeAt = null;
    this.canvas.style.cursor = tool === "blade" ? "crosshair" : "default";
    this.options.onChange();
    this.invalidate();
  }

  setSnapping(on: boolean): void {
    this.snapping = on;
    this.options.onChange();
  }

  scrollRowsBy(dy: number): void {
    this.setScroll(this.viewport.t, this.scrollY + dy);
  }

  private resize(): void {
    this.dpr = devicePixelRatio || 1;
    this.width = this.host.clientWidth;
    this.height = this.host.clientHeight;
    this.canvas.width = Math.max(1, Math.round(this.width * this.dpr));
    this.canvas.height = Math.max(1, Math.round(this.height * this.dpr));
    this.canvas.style.width = `${this.width}px`;
    this.canvas.style.height = `${this.height}px`;
    this.redraw();
  }

  private retheme(): void {
    this.theme = resolvedTheme();
    this.invalidate();
  }

  private followPlayhead(t: number): void {
    const x = timeToX(this.viewport, t);
    if (x >= 0 && x <= this.width - 24) return;
    this.setScroll(t - this.width * 0.1 * this.viewport.zoom);
  }

  private frame(): number {
    const sequence = this.sequence;
    return sequence ? flicksPerFrame(sequence.fps) : 1;
  }

  private snapFrame(t: number): number {
    const frame = this.frame();
    return Math.round(t / frame) * frame;
  }

  private targets(exclude: readonly string[] = [], base = this.sequence): number[] {
    if (!base || !this.snapping) return [];
    return snapTargets(base, { exclude, extra: [this.options.view.get().playhead] });
  }

  private dispatch(command: Command): boolean {
    const result = this.options.store.dispatch(command);
    if (!result.ok) this.options.onError(result.error.message);
    return result.ok;
  }

  private local(e: { clientX: number; clientY: number }) {
    const rect = this.canvas.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  }

  /** Content y (track rows, scroll included) for a canvas y. */
  private contentY(y: number): number {
    return y - RULER_HEIGHT + this.scrollY;
  }

  private select(ids: readonly string[]): void {
    this.options.view.set({ selection: ids });
  }

  private get selection(): readonly string[] {
    return this.options.view.get().selection;
  }

  /** Selected clips in this sequence that are on unlocked tracks. */
  private editable(ids: readonly string[]): string[] {
    const wanted = new Set(ids);
    const found: string[] = [];
    for (const track of this.sequence?.tracks ?? []) {
      if (track.locked) continue;
      for (const clip of track.clips) if (wanted.has(clip.id)) found.push(clip.id);
    }
    return found;
  }

  // --------------------------------------------------------------- pointer

  private onDown(e: PointerEvent): void {
    if (e.button !== 0 && e.button !== 1) return;
    this.canvas.focus();
    const { x, y } = this.local(e);
    this.pointer = { x, y, ctrl: e.ctrlKey || e.metaKey };
    const view = this.viewport;
    const readOnly = this.options.readOnly();

    if (e.button === 1 || y >= this.height - SCROLLBAR) {
      this.capture(e);
      if (y >= this.height - SCROLLBAR) this.jumpScrollbar(x);
      this.drag = { kind: "scrollbar", startX: x, startT: this.viewport.t };
      return;
    }
    if (y < RULER_HEIGHT) {
      this.capture(e);
      this.follow = true;
      this.drag = { kind: "scrub" };
      this.options.view.set({ scrubbing: true });
      this.scrubTo(x);
      return;
    }

    const rows = this.rows;
    const cy = this.contentY(y);
    const hit = hitClip(rows, view, x, cy);

    if (this.tool === "blade") {
      if (hit && !readOnly && !hit.row.track.locked) {
        const t = this.bladeTime(x);
        if (t > hit.clip.start && t < clipEnd(hit.clip))
          this.dispatch({ type: "clip.split", payload: { clips: [hit.clip.id], t } });
      }
      return;
    }

    if (!hit) {
      this.capture(e);
      this.drag = {
        kind: "box",
        startX: x,
        startY: cy,
        startT: xToTime(view, x),
        startScrollY: this.scrollY,
        add: e.shiftKey || e.ctrlKey || e.metaKey ? this.selection : [],
        moved: false,
      };
      return;
    }

    const id = hit.clip.id;
    let selection = this.selection;
    if (e.shiftKey || e.ctrlKey || e.metaKey) {
      selection = selection.includes(id) ? selection.filter((s) => s !== id) : [...selection, id];
      this.select(selection);
      return;
    }
    if (!selection.includes(id)) {
      selection = [id];
      this.select(selection);
    }
    const sequence = this.sequence;
    if (readOnly || hit.row.track.locked || !sequence) return;
    const clips = this.editable(selection);
    if (!clips.includes(id)) return;

    this.capture(e);
    if (hit.zone === "body") {
      this.drag = {
        kind: "pending-move",
        startX: x,
        startY: cy,
        startT: xToTime(view, x),
        rows,
        startRow: hit.row,
        clips,
        primary: hit.clip,
        base: sequence,
        gesture: null,
        payload: null,
        trackOffset: 0,
      };
    } else {
      const gesture = this.begin("clip-trim", "clip.trim");
      if (!gesture) return;
      this.drag = {
        kind: "trim",
        startT: xToTime(view, x),
        edge: hit.zone,
        clips,
        primary: hit.clip,
        base: sequence,
        gesture,
        payload: null,
      };
    }
  }

  private begin(key: string, type: string): Gesture | null {
    try {
      return this.options.store.beginGesture(key, type);
    } catch (error) {
      this.options.onError(error instanceof Error ? error.message : String(error));
      return null;
    }
  }

  private capture(e: PointerEvent): void {
    try {
      this.canvas.setPointerCapture(e.pointerId);
    } catch {
      // synthetic events in tests have no active pointer
    }
  }

  private onMove(e: PointerEvent): void {
    const { x, y } = this.local(e);
    this.pointer = { x, y, ctrl: e.ctrlKey || e.metaKey };
    if (!this.drag) return this.hover(x, y);
    this.update();
  }

  /** Applies the current drag for the last pointer position (also after auto-scroll). */
  private update(): void {
    const drag = this.drag;
    if (!drag) return;
    const { x, y, ctrl } = this.pointer;
    const view = this.viewport;
    const t = xToTime(view, x);

    switch (drag.kind) {
      case "scrub":
        this.scrubTo(x);
        return;
      case "scrollbar": {
        const total = this.scrollTotal();
        this.follow = false;
        this.setScroll(drag.startT + ((x - drag.startX) / this.width) * total);
        return;
      }
      case "box": {
        const cy = this.contentY(y);
        if (!drag.moved && Math.hypot(t - drag.startT, cy - drag.startY) === 0) return;
        drag.moved = true;
        const x0 = timeToX(view, drag.startT);
        this.box = { x0, y0: drag.startY, x1: x, y1: cy };
        const inside = clipsInBox(this.rows, view, this.box);
        this.select([...new Set([...drag.add, ...inside])]);
        this.invalidate();
        return;
      }
      case "pending-move":
      case "move": {
        const cy = this.contentY(y);
        if (drag.kind === "pending-move") {
          const dx = (t - drag.startT) / view.zoom;
          if (Math.abs(dx) < DRAG_THRESHOLD && Math.abs(cy - drag.startY) < DRAG_THRESHOLD) return;
          const gesture = this.begin("clip-move", "clip.move");
          if (!gesture) {
            this.drag = null;
            return;
          }
          Object.assign(drag, { kind: "move", gesture });
          this.canvas.style.cursor = "grabbing";
        }
        if (!drag.gesture) return;
        let delta = t - drag.startT;
        const snap = snapSpan(
          this.targets(drag.clips, drag.base),
          drag.primary.start + delta,
          drag.primary.duration,
          SNAP_PX * view.zoom,
        );
        delta += snap.shift;
        this.snapLine = snap.hit;
        const earliest = Math.min(
          ...drag.base.tracks.flatMap((tr) =>
            tr.clips.filter((c) => drag.clips.includes(c.id)).map((c) => c.start),
          ),
        );
        delta = Math.max(this.snapFrame(delta), -earliest);
        const row = rowAt(drag.rows, Math.max(0, cy)) ?? drag.rows[drag.rows.length - 1];
        if (row && row.track.kind === drag.startRow.track.kind)
          drag.trackOffset = row.index - drag.startRow.index;
        const payload = {
          clips: drag.clips,
          delta,
          tracks: drag.trackOffset,
          mode: ctrl ? "insert" : "overwrite",
        };
        if (drag.gesture.preview(payload).ok) drag.payload = payload;
        this.invalidate();
        return;
      }
      case "trim": {
        const original = drag.edge === "start" ? drag.primary.start : clipEnd(drag.primary);
        const raw = original + (t - drag.startT);
        const snapped = snapNearest(this.targets(drag.clips, drag.base), raw, SNAP_PX * view.zoom);
        this.snapLine = snapped.hit;
        const delta = this.snapFrame(snapped.t) - original;
        const payload = { clips: drag.clips, edge: drag.edge, delta };
        if (drag.gesture.preview(payload).ok) drag.payload = payload;
        // The viewer shows the frame at the edge being trimmed.
        const trimmed = this.sequence?.tracks
          .flatMap((track) => track.clips)
          .find((clip) => clip.id === drag.primary.id);
        if (trimmed) {
          const edge = drag.edge === "start" ? trimmed.start : clipEnd(trimmed) - this.frame();
          this.options.view.set({ peek: Math.max(0, edge) });
        }
        this.invalidate();
        return;
      }
    }
  }

  private onUp(e: PointerEvent | null): void {
    const drag = this.drag;
    this.drag = null;
    this.snapLine = null;
    this.box = null;
    if (e && this.canvas.hasPointerCapture(e.pointerId))
      this.canvas.releasePointerCapture(e.pointerId);
    this.canvas.style.cursor = this.tool === "blade" ? "crosshair" : "default";
    if (!drag) return;
    this.endViewerDrag(drag);
    if (drag.kind === "box" && !drag.moved) {
      // A click on empty space: clear the selection and move the playhead there.
      this.select(drag.add);
      this.follow = true;
      this.options.view.set({ playhead: this.snapFrame(Math.max(0, drag.startT)) });
    }
    if ((drag.kind === "move" || drag.kind === "trim") && drag.gesture) {
      if (drag.payload === null) drag.gesture.cancel();
      else {
        const result = drag.gesture.commit(drag.payload);
        if (!result.ok) this.options.onError(result.error.message);
      }
    }
    this.invalidate();
  }

  /** The viewer goes back to the playhead once a scrub or trim ends. */
  private endViewerDrag(drag: Drag): void {
    if (drag.kind === "scrub") this.options.view.set({ scrubbing: false });
    if (drag.kind === "trim") this.options.view.set({ peek: null });
  }

  /** Puts everything back as it was before the drag. */
  cancelDrag(): void {
    const drag = this.drag;
    this.drag = null;
    this.snapLine = null;
    this.box = null;
    if (!drag) return;
    this.endViewerDrag(drag);
    if ((drag.kind === "move" || drag.kind === "trim") && drag.gesture) drag.gesture.cancel();
    if (drag.kind === "box") this.select(drag.add);
    this.canvas.style.cursor = this.tool === "blade" ? "crosshair" : "default";
    this.invalidate();
  }

  private hover(x: number, y: number): void {
    if (y < RULER_HEIGHT || y >= this.height - SCROLLBAR) {
      this.canvas.style.cursor = y < RULER_HEIGHT ? "text" : "default";
      if (this.bladeAt !== null) {
        this.bladeAt = null;
        this.invalidate();
      }
      return;
    }
    if (this.tool === "blade") {
      const hit = hitClip(this.rows, this.viewport, x, this.contentY(y));
      const at = hit ? this.bladeTime(x) : null;
      if (at !== this.bladeAt) {
        this.bladeAt = at;
        this.invalidate();
      }
      this.canvas.style.cursor = "crosshair";
      return;
    }
    const hit = hitClip(this.rows, this.viewport, x, this.contentY(y));
    const editable = hit && !hit.row.track.locked && !this.options.readOnly();
    this.canvas.style.cursor = !hit
      ? "default"
      : !editable
        ? "default"
        : hit.zone === "body"
          ? "grab"
          : "ew-resize";
  }

  private bladeTime(x: number): number {
    const view = this.viewport;
    const t = xToTime(view, x);
    return this.snapFrame(snapNearest(this.targets(), t, SNAP_PX * view.zoom).t);
  }

  private scrubTo(x: number): void {
    const view = this.viewport;
    const raw = Math.max(0, xToTime(view, x));
    const sequence = this.sequence;
    const targets = sequence && this.snapping ? snapTargets(sequence) : [];
    const snapped = snapNearest(targets, raw, SNAP_PX * view.zoom);
    this.snapLine = snapped.hit;
    this.options.view.set({ playhead: this.snapFrame(snapped.t) });
  }

  private scrollTotal(): number {
    const sequence = this.sequence;
    const end = sequence ? sequenceDuration(sequence) : 0;
    return Math.max(end * 1.1, this.width * this.viewport.zoom * 1.5);
  }

  private jumpScrollbar(x: number): void {
    const total = this.scrollTotal();
    const visible = this.width * this.viewport.zoom;
    this.follow = false;
    this.setScroll((x / this.width) * total - visible / 2);
  }

  private autoScroll(): void {
    const drag = this.drag;
    if (!drag || !["box", "move", "trim"].includes(drag.kind)) return;
    const { x, y } = this.pointer;
    const dx = edgeScroll(x, 0, this.width);
    const dy = drag.kind === "trim" ? 0 : edgeScroll(y, RULER_HEIGHT, this.height - SCROLLBAR);
    if (dx === 0 && dy === 0) return;
    const view = this.viewport;
    this.follow = false;
    this.setScroll(view.t + dx * view.zoom, this.scrollY + dy);
    this.update();
  }

  private onWheel(e: WheelEvent): void {
    e.preventDefault();
    const intent = wheelIntent(e as WheelEvent & { wheelDeltaY?: number; wheelDeltaX?: number });
    if (intent.kind === "zoom") {
      this.zoomBy(intent.factor, this.local(e).x);
      return;
    }
    this.follow = false;
    const view = this.viewport;
    this.setScroll(view.t + intent.dx * view.zoom, this.scrollY + intent.dy);
    if (this.drag) this.update();
  }

  // --------------------------------------------------------------- keyboard

  private onKey(e: KeyboardEvent): void {
    const mod = e.ctrlKey || e.metaKey;
    const key = e.key;
    const readOnly = this.options.readOnly();
    let handled = true;
    if ((key === "Delete" || key === "Backspace") && !mod) {
      if (!readOnly) this.deleteSelected(e.shiftKey);
    } else if (key.toLowerCase() === "s" && !mod) {
      if (!readOnly) this.splitAtPlayhead();
    } else if (key.toLowerCase() === "v" && !mod) this.setTool("select");
    else if (key.toLowerCase() === "b" && !mod) this.setTool("blade");
    else if (key.toLowerCase() === "a" && mod) this.selectAll();
    else if (key === "Escape") this.select([]);
    else if (key === "=" || key === "+") this.zoomBy(1 / 1.5);
    else if (key === "-" || key === "_") this.zoomBy(1.5);
    else if (key === "\\" || (key.toLowerCase() === "z" && e.shiftKey && !mod)) this.fit();
    else if (key === "ArrowLeft" || key === "ArrowRight") {
      const step = this.frame() * (e.shiftKey ? 10 : 1) * (key === "ArrowLeft" ? -1 : 1);
      this.follow = true;
      this.options.view.set({
        playhead: Math.max(0, this.snapFrame(this.options.view.get().playhead + step)),
      });
    } else if (key === "Home") {
      this.follow = true;
      this.options.view.set({ playhead: 0 });
    } else if (key === "End") {
      const sequence = this.sequence;
      this.follow = true;
      if (sequence) this.options.view.set({ playhead: sequenceDuration(sequence) });
    } else handled = false;
    if (handled) e.preventDefault();
  }

  deleteSelected(ripple: boolean): void {
    const clips = this.editable(this.selection);
    if (clips.length === 0) return;
    if (this.dispatch({ type: ripple ? "clip.rippleDelete" : "clip.delete", payload: { clips } }))
      this.select([]);
  }

  /** Splits the selected clips at the playhead; with nothing selected, every clip under it. */
  splitAtPlayhead(): void {
    const sequence = this.sequence;
    if (!sequence) return;
    const t = this.snapFrame(this.options.view.get().playhead);
    const under = (ids: Set<string> | null) =>
      sequence.tracks
        .filter((track) => !track.locked)
        .flatMap((track) => track.clips)
        .filter((c) => (ids === null || ids.has(c.id)) && c.start < t && t < clipEnd(c))
        .map((c) => c.id);
    const selected = under(new Set(this.selection));
    const clips = selected.length > 0 ? selected : under(null);
    if (clips.length > 0) this.dispatch({ type: "clip.split", payload: { clips, t } });
  }

  selectAll(): void {
    this.select(this.sequence?.tracks.flatMap((t) => t.clips.map((c) => c.id)) ?? []);
  }

  // --------------------------------------------------------------- drop from the bin

  private dropTarget(e: DragEvent): Ghost | null {
    const id = draggedMedia(e);
    const media = id ? this.project.media.find((m) => m.id === id) : undefined;
    const sequence = this.sequence;
    if (!media || !sequence || this.options.readOnly()) return null;
    const kind: Track["kind"] = media.video ? "video" : "audio";
    const { x, y } = this.local(e);
    const rows = this.rows;
    const under = rowAt(rows, this.contentY(y));
    const fits = (row: Row | undefined) =>
      row !== undefined && row.track.kind === kind && !row.track.locked;
    const row = (fits(under) ? under : rows.find(fits)) ?? null;
    const frame = this.frame();
    const duration = Math.floor(media.duration / frame) * frame;
    if (duration < frame) return null;
    const view = this.viewport;
    const raw = Math.max(0, xToTime(view, x));
    const snap = snapSpan(this.targets(), raw, duration, SNAP_PX * view.zoom);
    this.snapLine = snap.hit;
    const start = Math.max(0, this.snapFrame(raw + snap.shift));
    return { media, row, start, duration };
  }

  private onDragOver(e: DragEvent): void {
    const ghost = this.dropTarget(e);
    if (!ghost) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    this.pointer.ctrl = e.ctrlKey || e.metaKey;
    this.ghost = ghost;
    this.invalidate();
  }

  private onDrop(e: DragEvent): void {
    const ghost = this.dropTarget(e);
    this.ghost = null;
    this.snapLine = null;
    this.invalidate();
    const sequence = this.sequence;
    if (!ghost || !sequence) return;
    e.preventDefault();
    this.addMedia(ghost.media, ghost.start, ghost.row?.track.id ?? null, e.ctrlKey || e.metaKey);
  }

  /** Puts a media item on a track (a new one when `trackId` is null) and selects the new clip. */
  addMedia(media: MediaItem, start: number, trackId: string | null, insert = false): boolean {
    const sequence = this.sequence;
    if (!sequence) return false;
    const frame = this.frame();
    const duration = Math.floor(media.duration / frame) * frame;
    const clipId = createId();
    const wasEmpty = sequenceDuration(sequence) === 0;
    let error: string | null = null;
    const ok = this.options.store.transaction(`Add ${media.name}`, () => {
      let track = trackId;
      if (track === null) {
        track = createId();
        const kind = media.video ? "video" : "audio";
        const added = this.options.store.dispatch({
          type: "track.add",
          payload: { sequence: sequence.id, id: track, kind },
        });
        if (!added.ok) return ((error = added.error.message), false);
      }
      const result = this.options.store.dispatch({
        type: "clip.add",
        payload: {
          track,
          mode: insert ? "insert" : "overwrite",
          clips: [{ id: clipId, media: media.id, start, duration }],
        },
      });
      if (!result.ok) error = result.error.message;
      return result.ok;
    });
    if (error) this.options.onError(error);
    if (ok) {
      this.select([clipId]);
      if (wasEmpty) this.fit();
    }
    return ok;
  }

  // --------------------------------------------------------------- drawing

  private draw(): void {
    const { ctx, width, height, dpr } = this;
    const c = colors[this.theme];
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = c.viewer;
    ctx.fillRect(0, 0, width, height);
    const sequence = this.sequence;
    if (!sequence) return;

    const view = this.viewport;
    const rows = this.rows;
    const from = view.t;
    const to = xToTime(view, width);
    const selected = new Set(this.selection);
    const wants: ThumbRequest[] = [];
    const top = RULER_HEIGHT;
    const areaBottom = height - SCROLLBAR;

    ctx.save();
    ctx.beginPath();
    ctx.rect(0, top, width, areaBottom - top);
    ctx.clip();

    for (const row of rows) {
      const y = top + row.y - this.scrollY;
      if (y > areaBottom || y + row.height < top) continue;
      ctx.fillStyle = c.border;
      ctx.fillRect(0, y + row.height, width, 1);
      const clips = row.track.clips;
      for (let k = firstVisible(clips, from); k < clips.length; k++) {
        const clip = clips[k];
        if (!clip || clip.start > to) break;
        this.drawClip(row, clip, y, view, selected.has(clip.id), wants);
      }
    }

    if (this.ghost) this.drawGhost(this.ghost, rows, view);
    if (this.box) {
      const x0 = Math.min(this.box.x0, this.box.x1);
      const y0 = top + Math.min(this.box.y0, this.box.y1) - this.scrollY;
      const w = Math.abs(this.box.x1 - this.box.x0);
      const h = Math.abs(this.box.y1 - this.box.y0);
      ctx.globalAlpha = 0.15;
      ctx.fillStyle = c.selection;
      ctx.fillRect(x0, y0, w, h);
      ctx.globalAlpha = 1;
      ctx.strokeStyle = c.selection;
      ctx.lineWidth = 1;
      ctx.strokeRect(Math.round(x0) + 0.5, Math.round(y0) + 0.5, Math.round(w), Math.round(h));
    }
    ctx.restore();
    this.thumbs.want(wants);

    this.drawRuler(sequence, view);

    // Snap guide and blade line.
    for (const [t, dashed] of [
      [this.snapLine, true],
      [this.bladeAt, false],
    ] as const) {
      if (t === null) continue;
      const x = Math.round(timeToX(view, t)) + 0.5;
      ctx.strokeStyle = c.selection;
      ctx.lineWidth = 1;
      ctx.setLineDash(dashed ? [4, 3] : []);
      ctx.beginPath();
      ctx.moveTo(x, top);
      ctx.lineTo(x, areaBottom);
      ctx.stroke();
      ctx.setLineDash([]);
    }

    // Playhead: a 1px line with a handle in the ruler.
    const px = Math.round(timeToX(view, this.options.view.get().playhead));
    if (px >= -6 && px <= width + 6) {
      ctx.fillStyle = c.playhead;
      ctx.fillRect(px, 6, 1, areaBottom - 6);
      ctx.beginPath();
      ctx.moveTo(px - 5, 4);
      ctx.lineTo(px + 6, 4);
      ctx.lineTo(px + 6, 12);
      ctx.lineTo(px + 0.5, 17);
      ctx.lineTo(px - 5, 12);
      ctx.closePath();
      ctx.fill();
    }

    this.drawScrollbar(view);
  }

  private font(style: keyof typeof textStyles): string {
    const t = textStyles[style];
    return `${t.fontWeight} ${t.fontSize}px ${fonts[t.family]}`;
  }

  private drawRuler(sequence: Sequence, view: View): void {
    const { ctx, width } = this;
    const c = colors[this.theme];
    ctx.fillStyle = c.surface;
    ctx.fillRect(0, 0, width, RULER_HEIGHT);
    ctx.fillStyle = c.border;
    ctx.fillRect(0, RULER_HEIGHT - 1, width, 1);

    const { major, minor } = rulerSteps(sequence, view.zoom);
    const withFrames = major < 705_600_000;
    const first = Math.max(0, Math.floor(view.t / minor));
    const last = Math.ceil(xToTime(view, width) / minor);
    ctx.fillStyle = c.borderStrong;
    for (let i = first; i <= last; i++) {
      const t = i * minor;
      const x = Math.round(timeToX(view, t));
      const isMajor = t % major === 0;
      const h = isMajor ? 10 : 5;
      ctx.fillRect(x, RULER_HEIGHT - 1 - h, 1, h);
    }
    // Labels: timecode style at 11px in text-tertiary.
    const tc = textStyles.timecode;
    ctx.font = `${tc.fontWeight} 11px ${fonts[tc.family]}`;
    ctx.fillStyle = c.textTertiary;
    ctx.textBaseline = "top";
    for (let i = Math.max(0, Math.floor(view.t / major)); i * major <= xToTime(view, width); i++) {
      const t = i * major;
      ctx.fillText(rulerLabel(t, sequence, withFrames), Math.round(timeToX(view, t)) + 4, 3);
    }
  }

  private drawScrollbar(view: View): void {
    const { ctx, width, height } = this;
    const c = colors[this.theme];
    const y = height - SCROLLBAR;
    ctx.fillStyle = c.surface;
    ctx.fillRect(0, y, width, SCROLLBAR);
    const total = this.scrollTotal();
    const x0 = (view.t / total) * width;
    const w = Math.max(24, ((width * view.zoom) / total) * width);
    ctx.fillStyle = c.groove;
    ctx.beginPath();
    ctx.roundRect(Math.min(x0, width - w), y + 2, w, SCROLLBAR - 4, radius.radiusSm);
    ctx.fill();
  }

  private drawGhost(ghost: Ghost, rows: Row[], view: View): void {
    const { ctx } = this;
    const c = colors[this.theme];
    const kind = ghost.media.video ? "video" : "audio";
    const y =
      RULER_HEIGHT + (ghost.row ? ghost.row.y : rowsHeight(rows)) - this.scrollY + CLIP_INSET;
    const h = (ghost.row?.height ?? (kind === "video" ? 72 : 48)) - CLIP_INSET * 2;
    const x0 = timeToX(view, ghost.start);
    const w = ghost.duration / view.zoom;
    ctx.globalAlpha = 0.5;
    ctx.fillStyle = kind === "video" ? c.clipVideo : c.clipAudio;
    ctx.beginPath();
    ctx.roundRect(x0, y, w, h, radius.radiusSm);
    ctx.fill();
    ctx.globalAlpha = 1;
    ctx.strokeStyle = c.selection;
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 3]);
    ctx.stroke();
    ctx.setLineDash([]);
  }

  private drawClip(
    row: Row,
    clip: Clip,
    rowY: number,
    view: View,
    selected: boolean,
    wants: ThumbRequest[],
  ): void {
    const { ctx, width } = this;
    const c = colors[this.theme];
    const gap = spacing.space2 / 2;
    const x0 = timeToX(view, clip.start) + gap;
    const x1 = timeToX(view, clipEnd(clip)) - gap;
    const y = rowY + CLIP_INSET;
    const h = row.height - CLIP_INSET * 2;
    const w = Math.max(1, x1 - x0);
    const kind = row.track.kind;
    const media =
      clip.media !== undefined ? this.project.media.find((m) => m.id === clip.media) : undefined;
    const state = media ? this.options.status.get(media.id) : undefined;
    const offline = state?.status === "offline";
    const dim = !clip.enabled || row.track.hidden;

    ctx.save();
    if (dim) ctx.globalAlpha = 0.5;
    ctx.beginPath();
    ctx.roundRect(x0, y, w, h, Math.min(radius.radiusSm, w / 2));
    ctx.fillStyle =
      kind === "video"
        ? c.clipVideo
        : kind === "audio"
          ? c.clipAudio
          : kind === "text"
            ? c.clipText
            : c.clipEffect;
    ctx.fill();
    if (w < 3) {
      ctx.restore();
      return;
    }
    ctx.clip();

    const hasAudioStrip = kind === "video" && media?.audio !== undefined;
    const stripTop = y + NAME_ROW;
    const stripBottom = y + h - (hasAudioStrip ? AUDIO_STRIP : 0);

    // Filmstrip, pinned to source time.
    if (kind === "video" && media?.video && !offline && stripBottom - stripTop > 8) {
      this.drawFilmstrip(clip, media, x0, Math.min(x1, width), stripTop, stripBottom, view, wants);
    }

    // Sound: a flat centre line until waveform peaks exist (M2).
    ctx.fillStyle = c.onClip;
    if (kind === "audio") {
      ctx.globalAlpha = (dim ? 0.5 : 1) * 0.7;
      ctx.fillRect(x0, Math.round(stripTop + (y + h - stripTop) / 2), w, 1);
    } else if (hasAudioStrip) {
      ctx.globalAlpha = (dim ? 0.5 : 1) * 0.25;
      ctx.fillRect(x0, stripBottom, w, 1);
      ctx.globalAlpha = (dim ? 0.5 : 1) * 0.7;
      ctx.fillRect(x0, Math.round(stripBottom + AUDIO_STRIP / 2), w, 1);
    }
    ctx.globalAlpha = dim ? 0.5 : 1;

    // Name, kept in view when the clip starts off screen.
    let textX = Math.max(x0, 0) + spacing.space6;
    const room = Math.min(x1, width) - textX - spacing.space6;
    if (room > 8) {
      ctx.textBaseline = "top";
      if (offline) {
        ctx.fillStyle = c.warning;
        drawIcon(ctx, "warning", textX, y + 3, 12, c.warning);
        ctx.font = this.font("caption");
        ctx.fillText("Offline", textX + 15, y + 3);
        textX += 15 + this.measure("Offline", this.font("caption")) + spacing.space6;
      }
      const name = media?.name ?? this.sequenceName(clip) ?? "";
      const left = Math.min(x1, width) - textX - spacing.space6;
      if (left > 8) {
        ctx.font = this.font("small");
        ctx.fillStyle = c.onClip;
        ctx.fillText(this.ellipsize(name, left, ctx.font), textX, y + 2);
      }
    }
    ctx.restore();

    if (selected) {
      ctx.save();
      ctx.beginPath();
      ctx.roundRect(x0 + 1, y + 1, w - 2, h - 2, Math.min(radius.radiusSm, w / 2));
      ctx.lineWidth = 2;
      ctx.strokeStyle = c.selection;
      ctx.stroke();
      ctx.beginPath();
      ctx.roundRect(x0 + 2.5, y + 2.5, w - 5, h - 5, Math.max(0, radius.radiusSm - 2));
      ctx.lineWidth = 1;
      ctx.strokeStyle = c.selectionEdge;
      ctx.stroke();
      ctx.restore();
    }
  }

  private sequenceName(clip: Clip): string | undefined {
    return clip.sequence === undefined
      ? undefined
      : this.project.sequences.find((s) => s.id === clip.sequence)?.name;
  }

  private drawFilmstrip(
    clip: Clip,
    media: MediaItem,
    x0: number,
    x1: number,
    top: number,
    bottom: number,
    view: View,
    wants: ThumbRequest[],
  ): void {
    const video = media.video;
    if (!video) return;
    const { ctx, dpr } = this;
    const stripH = bottom - top;
    const sideways = video.rotation === 90 || video.rotation === 270;
    const aspect = sideways ? video.height / video.width : video.width / video.height;
    const tileW = stripH * aspect;
    // Source flicks per screen pixel.
    const perPx = view.zoom * clip.speed;
    // Tiles sit on a grid of source time (one tile = `span` source flicks), so trimming reveals or
    // hides tiles instead of sliding the pictures. Each tile shows the cached picture of the
    // power-of-two slot under its middle; slots are never wider than a tile, so neighbouring
    // tiles get their own pictures, and the keys stay the same across nearby zoom levels.
    const span = tileW * perPx;
    const level = Math.floor(Math.log2(Math.max(1, span)));
    const slot = 2 ** level;
    const left = Math.max(x0, 0);
    const right = Math.min(x1, this.width);
    if (right <= left) return;
    const xAt = (s: number) => x0 - 1 + (s - clip.in) / perPx;
    const sourceAt = (x: number) => clip.in + (x - (x0 - 1)) * perPx;
    const centre = this.width / 2;
    const width = Math.max(2, Math.round(tileW * dpr));
    const height = Math.max(2, Math.round(stripH * dpr));
    const last = Math.max(0, media.duration - 1);
    for (let m = Math.floor(sourceAt(left) / span); xAt(m * span) < right; m++) {
      const tx = xAt(m * span);
      const k = Math.floor((m * span + span / 2) / slot);
      const key = thumbKey(media.id, level, k);
      let bitmap = this.thumbs.get(key);
      if (!bitmap) {
        wants.push({
          key,
          media: media.id,
          time: Math.min(Math.max(0, k * slot + slot / 2), last),
          priority: Math.abs(tx + tileW / 2 - centre),
          width,
          height,
          rotation: video.rotation,
        });
        // A coarser picture already cached stands in while this one loads.
        for (let up = 1; !bitmap && up <= 6; up++)
          bitmap = this.thumbs.get(thumbKey(media.id, level + up, Math.floor(k / 2 ** up)));
      }
      // The clip's own shape cuts the first and last tiles; tiles are never squeezed, and a 1px
      // gap in the clip colour separates them.
      if (bitmap) ctx.drawImage(bitmap, tx, top, tileW - 1, stripH);
    }
  }

  private measure(text: string, font: string): number {
    const key = `${font}\u0000${text}`;
    let w = this.textWidths.get(key);
    if (w === undefined) {
      this.ctx.font = font;
      w = this.ctx.measureText(text).width;
      if (this.textWidths.size > 4000) this.textWidths.clear();
      this.textWidths.set(key, w);
    }
    return w;
  }

  /** `text`, cut with an ellipsis to fit `room` pixels. */
  private ellipsize(text: string, room: number, font: string): string {
    if (this.measure(text, font) <= room) return text;
    let lo = 0;
    let hi = text.length;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this.measure(`${text.slice(0, mid)}…`, font) <= room) lo = mid;
      else hi = mid - 1;
    }
    return lo === 0 ? "" : `${text.slice(0, lo)}…`;
  }
}

const iconPaths = new Map<string, Path2D[]>();

/** Draws a design-system icon (24-unit grid, 1.5px line at 16px) at `size` pixels. */
function drawIcon(
  ctx: CanvasRenderingContext2D,
  name: keyof typeof ICONS,
  x: number,
  y: number,
  size: number,
  color: string,
): void {
  let paths = iconPaths.get(name);
  if (!paths) {
    paths = ICONS[name]
      .filter(([tag]) => tag === "path")
      .map(([, attrs]) => new Path2D(attrs["d"] ?? ""));
    iconPaths.set(name, paths);
  }
  ctx.save();
  ctx.translate(x, y);
  ctx.scale(size / 24, size / 24);
  ctx.strokeStyle = color;
  ctx.lineWidth = 2.25;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  for (const path of paths) ctx.stroke(path);
  ctx.restore();
}
