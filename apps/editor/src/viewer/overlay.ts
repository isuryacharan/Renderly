// The viewer overlay: a canvas over the picture with the selected clips' outlines and, for one
// clip, handles to move, scale and rotate it; snapping guides to the frame's centre and edges;
// optional safe areas and a rule-of-thirds grid. Clicking a clip in the picture selects it.
//
// Shapes come from the evaluator at the playhead (core `frameAt`), so they match what plays. A
// drag is one store gesture on `component.setMany`: one undo step, Escape cancels.

import {
  evaluateSequence,
  frameAt,
  type FrameLayer,
  type Gesture,
  type LayerTransform,
  type ProjectStore,
  type Sequence,
  type ViewStore,
} from "@renderly/core";
import { colors } from "@renderly/ui/tokens";
import { viewRect, type ViewerView } from "@renderly/engine";
import { onThemeChange, resolvedTheme } from "../theme";

type Point = { x: number; y: number };
type Handle = "move" | "rotate" | `scale-${"n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw"}`;

const HANDLE = 8;
const HIT = 10;
const ROTATE_OFFSET = 28;
/** CSS pixels within which a moved clip snaps to the frame's centre or edges. */
const SNAP = 6;

export interface OverlayOptions {
  store: ProjectStore;
  view: ViewStore;
  /** How the viewer shows the picture (fit, zoom, pan). */
  viewer: () => ViewerView;
  readOnly: () => boolean;
  onError: (message: string) => void;
  /** Pauses playback (a drag starts). */
  pause: () => void;
}

interface Drag {
  handle: Handle;
  start: Point;
  gesture: Gesture;
  clips: string[];
  /** Transforms at the start, per clip. */
  from: LayerTransform[];
  /** Screen position of the primary clip's anchor at the start. */
  anchor: Point;
  payload: unknown;
}

export class ViewerOverlay {
  readonly canvas = document.createElement("canvas");
  private readonly ctx: CanvasRenderingContext2D;
  private width = 0;
  private height = 0;
  private dpr = 1;
  private dirty = true;
  private frameId = 0;
  private drag: Drag | null = null;
  private guides: { x: number[]; y: number[] } = { x: [], y: [] };
  private readonly cleanup: (() => void)[] = [];
  safeArea = false;
  thirds = false;

  constructor(
    private readonly host: HTMLElement,
    private readonly options: OverlayOptions,
  ) {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) throw new Error("No 2D canvas");
    this.ctx = ctx;
    this.canvas.className = "viewer__overlay";
    host.appendChild(this.canvas);
    const resize = new ResizeObserver(() => this.resize());
    resize.observe(host);
    this.cleanup.push(() => resize.disconnect());
    this.cleanup.push(options.store.subscribe(() => this.invalidate()));
    this.cleanup.push(options.view.subscribe(() => this.invalidate()));
    this.cleanup.push(onThemeChange(() => this.invalidate()));
    const on = <K extends keyof HTMLElementEventMap>(
      type: K,
      listener: (event: HTMLElementEventMap[K]) => void,
    ) => {
      this.canvas.addEventListener(type, listener);
      this.cleanup.push(() => this.canvas.removeEventListener(type, listener));
    };
    on("pointerdown", (e) => this.onDown(e));
    on("pointermove", (e) => this.onMove(e));
    on("pointerup", () => this.onUp());
    on("pointercancel", () => this.cancel());
    const escape = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !this.drag) return;
      e.preventDefault();
      e.stopPropagation();
      this.cancel();
    };
    addEventListener("keydown", escape, true);
    this.cleanup.push(() => removeEventListener("keydown", escape, true));
    this.resize();
    const loop = () => {
      this.frameId = requestAnimationFrame(loop);
      if (!this.dirty) return;
      this.dirty = false;
      this.draw();
    };
    this.frameId = requestAnimationFrame(loop);
  }

  destroy(): void {
    cancelAnimationFrame(this.frameId);
    this.cancel();
    for (const fn of this.cleanup) fn();
    this.canvas.remove();
  }

  invalidate(): void {
    this.dirty = true;
  }

  // --- geometry ---

  private resize(): void {
    this.dpr = devicePixelRatio || 1;
    this.width = this.host.clientWidth;
    this.height = this.host.clientHeight;
    this.canvas.width = Math.max(1, Math.round(this.width * this.dpr));
    this.canvas.height = Math.max(1, Math.round(this.height * this.dpr));
    this.canvas.style.width = `${this.width}px`;
    this.canvas.style.height = `${this.height}px`;
    this.invalidate();
  }

  private get sequence(): Sequence | undefined {
    const id = this.options.view.get().sequence;
    const project = this.options.store.project;
    return project.sequences.find((s) => s.id === id) ?? project.sequences[0];
  }

  /** The picture's rectangle in CSS pixels (top-left origin), as the renderer draws it. */
  picture(): { x: number; y: number; width: number; height: number } | null {
    const sequence = this.sequence;
    if (!sequence) return null;
    const device = {
      width: Math.round(this.width * this.dpr),
      height: Math.round(this.height * this.dpr),
    };
    const r = viewRect(sequence, device, this.options.viewer());
    return {
      x: r.x / this.dpr,
      y: (device.height - r.y - r.height) / this.dpr,
      width: r.width / this.dpr,
      height: r.height / this.dpr,
    };
  }

  /** Layers showing at the playhead, bottom first. */
  private layers(): FrameLayer[] {
    const sequence = this.sequence;
    if (!sequence) return [];
    const graph = evaluateSequence(this.options.store.project, sequence.id);
    return frameAt(graph, this.options.view.get().playhead).layers;
  }

  /** A point of the layer (fractions from its centre, before mirroring) on screen. */
  private toScreen(t: LayerTransform, lx: number, ly: number): Point | null {
    const p = this.picture();
    if (!p) return null;
    const a = (t.rotation * Math.PI) / 180;
    // Inverse of the compositor's mapping: layer → pixels from the anchor's place.
    const sx = (lx - t.anchorX) * t.scaleX * p.width;
    const sy = (ly - t.anchorY) * t.scaleY * p.height;
    const qx = Math.cos(a) * sx - Math.sin(a) * sy;
    const qy = Math.sin(a) * sx + Math.cos(a) * sy;
    return {
      x: p.x + (0.5 + t.x) * p.width + qx,
      y: p.y + (0.5 + t.y) * p.height + qy,
    };
  }

  private corners(t: LayerTransform): Point[] {
    return (
      [
        [-0.5, -0.5],
        [0.5, -0.5],
        [0.5, 0.5],
        [-0.5, 0.5],
      ] as const
    )
      .map(([x, y]) => this.toScreen(t, x, y))
      .filter((p): p is Point => p !== null);
  }

  private handles(t: LayerTransform): { handle: Handle; at: Point }[] {
    const list: [Handle, number, number][] = [
      ["scale-nw", -0.5, -0.5],
      ["scale-n", 0, -0.5],
      ["scale-ne", 0.5, -0.5],
      ["scale-e", 0.5, 0],
      ["scale-se", 0.5, 0.5],
      ["scale-s", 0, 0.5],
      ["scale-sw", -0.5, 0.5],
      ["scale-w", -0.5, 0],
    ];
    const out = list
      .map(([handle, x, y]) => ({ handle, at: this.toScreen(t, x, y) }))
      .filter((h): h is { handle: Handle; at: Point } => h.at !== null);
    // The rotate handle sits above the top edge, along the clip's own up.
    const top = this.toScreen(t, 0, -0.5);
    const centre = this.toScreen(t, 0, 0);
    if (top && centre) {
      const dx = top.x - centre.x;
      const dy = top.y - centre.y;
      const length = Math.hypot(dx, dy) || 1;
      out.push({
        handle: "rotate",
        at: { x: top.x + (dx / length) * ROTATE_OFFSET, y: top.y + (dy / length) * ROTATE_OFFSET },
      });
    }
    return out;
  }

  private static inside(point: Point, quad: Point[]): boolean {
    let inside = false;
    for (let i = 0, j = quad.length - 1; i < quad.length; j = i++) {
      const a = quad[i] as Point;
      const b = quad[j] as Point;
      if (
        a.y > point.y !== b.y > point.y &&
        point.x < ((b.x - a.x) * (point.y - a.y)) / (b.y - a.y) + a.x
      )
        inside = !inside;
    }
    return inside;
  }

  private local(e: { clientX: number; clientY: number }): Point {
    const r = this.canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  // --- pointer ---

  private selectedLayers(layers = this.layers()): FrameLayer[] {
    const selected = new Set(this.options.view.get().selection);
    return layers.filter((l) => selected.has(l.clip));
  }

  private onDown(e: PointerEvent): void {
    if (e.button !== 0 || this.options.readOnly()) return;
    const point = this.local(e);
    const layers = this.layers();
    let selected = this.selectedLayers(layers);
    let handle: Handle | null = null;
    if (selected.length === 1 && selected[0]) {
      const near = this.handles(selected[0].transform).find(
        (h) => Math.hypot(h.at.x - point.x, h.at.y - point.y) <= HIT,
      );
      if (near) handle = near.handle;
    }
    if (!handle && selected.some((l) => ViewerOverlay.inside(point, this.corners(l.transform))))
      handle = "move";
    if (!handle) {
      // A click on another clip in the picture selects it (the topmost), and can drag it.
      const hit = [...layers]
        .reverse()
        .find((l) => ViewerOverlay.inside(point, this.corners(l.transform)));
      if (!hit) return; // empty space: let the viewer pan
      this.options.view.set({ selection: [hit.clip] });
      selected = [hit];
      handle = "move";
    }
    e.stopPropagation();
    e.preventDefault();
    const primary = selected[selected.length - 1];
    if (!primary) return;
    let gesture: Gesture;
    try {
      gesture = this.options.store.beginGesture(`viewer:${handle}`, "component.setMany");
    } catch (error) {
      this.options.onError(error instanceof Error ? error.message : String(error));
      return;
    }
    if (this.options.view.get().playing) this.options.pause();
    this.canvas.setPointerCapture(e.pointerId);
    this.drag = {
      handle,
      start: point,
      gesture,
      clips: selected.map((l) => l.clip),
      from: selected.map((l) => l.transform),
      anchor:
        this.toScreen(primary.transform, primary.transform.anchorX, primary.transform.anchorY) ??
        point,
      payload: null,
    };
  }

  private onMove(e: PointerEvent): void {
    const point = this.local(e);
    const drag = this.drag;
    if (!drag) {
      this.hover(point);
      return;
    }
    const p = this.picture();
    if (!p) return;
    const primary = drag.from[drag.from.length - 1];
    if (!primary) return;
    this.guides = { x: [], y: [] };
    let payload: Record<string, unknown>;
    if (drag.handle === "move") {
      let dx = (point.x - drag.start.x) / p.width;
      let dy = (point.y - drag.start.y) / p.height;
      if (!e.altKey) {
        // Snap the primary clip's centre to the frame's centre, and its box to the frame's edges.
        const snapped = this.snapMove(primary, dx, dy);
        dx = snapped.dx;
        dy = snapped.dy;
      }
      payload = {
        clips: drag.clips,
        component: "transform",
        field: "position",
        values: drag.from.map((t) => [t.x + dx, t.y + dy]),
      };
    } else if (drag.handle === "rotate") {
      const angle = (q: Point) => Math.atan2(q.y - drag.anchor.y, q.x - drag.anchor.x);
      let delta = ((angle(point) - angle(drag.start)) * 180) / Math.PI;
      let next = primary.rotation + delta;
      if (e.shiftKey) next = Math.round(next / 15) * 15;
      delta = next - primary.rotation;
      payload = {
        clips: drag.clips,
        component: "transform",
        field: "rotation",
        values: drag.from.map((t) => Math.round((t.rotation + delta) * 10) / 10),
      };
    } else {
      // Scale in the clip's own axes: corners scale both (keeping the ratio), edges one.
      const a = (primary.rotation * Math.PI) / 180;
      const local = (q: Point) => {
        const x = q.x - drag.anchor.x;
        const y = q.y - drag.anchor.y;
        return { x: Math.cos(a) * x + Math.sin(a) * y, y: -Math.sin(a) * x + Math.cos(a) * y };
      };
      const s = local(drag.start);
      const n = local(point);
      const ratio = (from: number, to: number) => (Math.abs(from) < 1 ? 1 : to / from);
      const side = drag.handle.slice(6);
      let fx = side.includes("e") || side.includes("w") ? ratio(s.x, n.x) : 1;
      let fy = side.includes("n") || side.includes("s") ? ratio(s.y, n.y) : 1;
      if (side.length === 2) {
        const f = Math.max(Math.abs(fx), Math.abs(fy));
        fx = f;
        fy = f;
      }
      const clampScale = (v: number) => Math.min(20, Math.max(0.01, Math.abs(v)));
      payload = {
        clips: drag.clips,
        component: "transform",
        field: "scale",
        values: drag.from.map((t) => [
          Math.round(clampScale(t.scaleX * fx) * 1000) / 1000,
          Math.round(clampScale(t.scaleY * fy) * 1000) / 1000,
        ]),
      };
    }
    if (drag.gesture.preview(payload).ok) drag.payload = payload;
    this.invalidate();
  }

  private snapMove(t: LayerTransform, dx: number, dy: number): { dx: number; dy: number } {
    const p = this.picture();
    if (!p) return { dx, dy };
    const moved = { ...t, x: t.x + dx, y: t.y + dy };
    const box = this.corners(moved);
    const xs = box.map((c) => c.x);
    const ys = box.map((c) => c.y);
    const anchor = this.toScreen(moved, t.anchorX, t.anchorY);
    const candidatesX = [
      ...(anchor ? [{ at: anchor.x, target: p.x + p.width / 2 }] : []),
      { at: Math.min(...xs), target: p.x },
      { at: Math.max(...xs), target: p.x + p.width },
    ];
    const candidatesY = [
      ...(anchor ? [{ at: anchor.y, target: p.y + p.height / 2 }] : []),
      { at: Math.min(...ys), target: p.y },
      { at: Math.max(...ys), target: p.y + p.height },
    ];
    const best = (list: { at: number; target: number }[]) =>
      list
        .map((c) => ({ ...c, d: c.target - c.at }))
        .filter((c) => Math.abs(c.d) <= SNAP)
        .sort((a, b) => Math.abs(a.d) - Math.abs(b.d))[0];
    const bx = best(candidatesX);
    const by = best(candidatesY);
    if (bx) this.guides.x.push(bx.target);
    if (by) this.guides.y.push(by.target);
    return { dx: dx + (bx ? bx.d / p.width : 0), dy: dy + (by ? by.d / p.height : 0) };
  }

  private onUp(): void {
    const drag = this.drag;
    this.drag = null;
    this.guides = { x: [], y: [] };
    if (!drag) return;
    if (drag.payload === null) drag.gesture.cancel();
    else {
      const result = drag.gesture.commit(drag.payload);
      if (!result.ok) this.options.onError(result.error.message);
    }
    this.invalidate();
  }

  /** Puts the clips back as they were before the drag. */
  cancel(): void {
    const drag = this.drag;
    this.drag = null;
    this.guides = { x: [], y: [] };
    drag?.gesture.cancel();
    this.invalidate();
  }

  private hover(point: Point): void {
    const selected = this.selectedLayers();
    let cursor = "";
    if (selected.length === 1 && selected[0]) {
      const near = this.handles(selected[0].transform).find(
        (h) => Math.hypot(h.at.x - point.x, h.at.y - point.y) <= HIT,
      );
      if (near) cursor = near.handle === "rotate" ? "grab" : "crosshair";
    }
    if (!cursor && selected.some((l) => ViewerOverlay.inside(point, this.corners(l.transform))))
      cursor = "move";
    this.canvas.style.cursor = cursor;
  }

  // --- drawing ---

  private draw(): void {
    const { ctx, dpr } = this;
    const c = colors[resolvedTheme()];
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, this.width, this.height);
    const p = this.picture();
    if (!p) return;

    if (this.thirds) {
      ctx.strokeStyle = "rgba(255, 255, 255, 0.35)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (const f of [1 / 3, 2 / 3]) {
        const x = Math.round(p.x + p.width * f) + 0.5;
        const y = Math.round(p.y + p.height * f) + 0.5;
        ctx.moveTo(x, p.y);
        ctx.lineTo(x, p.y + p.height);
        ctx.moveTo(p.x, y);
        ctx.lineTo(p.x + p.width, y);
      }
      ctx.stroke();
    }
    if (this.safeArea) {
      ctx.strokeStyle = "rgba(255, 255, 255, 0.45)";
      ctx.setLineDash([4, 4]);
      for (const share of [0.9, 0.8]) {
        const w = p.width * share;
        const h = p.height * share;
        ctx.strokeRect(
          Math.round(p.x + (p.width - w) / 2) + 0.5,
          Math.round(p.y + (p.height - h) / 2) + 0.5,
          Math.round(w),
          Math.round(h),
        );
      }
      ctx.setLineDash([]);
    }

    const selected = this.selectedLayers();
    for (const layer of selected) {
      const quad = this.corners(layer.transform);
      if (quad.length < 4) continue;
      ctx.beginPath();
      quad.forEach((q, i) => (i === 0 ? ctx.moveTo(q.x, q.y) : ctx.lineTo(q.x, q.y)));
      ctx.closePath();
      ctx.lineWidth = 3;
      ctx.strokeStyle = c.selectionEdge;
      ctx.stroke();
      ctx.lineWidth = 1.5;
      ctx.strokeStyle = c.selection;
      ctx.stroke();
    }
    if (selected.length === 1 && selected[0]) {
      const t = selected[0].transform;
      const handles = this.handles(t);
      const top = this.toScreen(t, 0, -0.5);
      const rotate = handles.find((h) => h.handle === "rotate");
      if (top && rotate) {
        ctx.beginPath();
        ctx.moveTo(top.x, top.y);
        ctx.lineTo(rotate.at.x, rotate.at.y);
        ctx.strokeStyle = c.selection;
        ctx.lineWidth = 1;
        ctx.stroke();
      }
      for (const { handle, at } of handles) {
        ctx.beginPath();
        if (handle === "rotate") ctx.arc(at.x, at.y, HANDLE / 2 + 1, 0, Math.PI * 2);
        else ctx.rect(at.x - HANDLE / 2, at.y - HANDLE / 2, HANDLE, HANDLE);
        ctx.fillStyle = c.selection;
        ctx.fill();
        ctx.lineWidth = 1;
        ctx.strokeStyle = c.selectionEdge;
        ctx.stroke();
      }
      // The anchor: a small cross.
      const anchor = this.toScreen(t, t.anchorX, t.anchorY);
      if (anchor) {
        ctx.beginPath();
        ctx.moveTo(anchor.x - 5, anchor.y);
        ctx.lineTo(anchor.x + 5, anchor.y);
        ctx.moveTo(anchor.x, anchor.y - 5);
        ctx.lineTo(anchor.x, anchor.y + 5);
        ctx.strokeStyle = c.selection;
        ctx.lineWidth = 1.5;
        ctx.stroke();
      }
    }

    // Snapping guides while moving.
    ctx.strokeStyle = c.selection;
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 3]);
    ctx.beginPath();
    for (const x of this.guides.x) {
      ctx.moveTo(Math.round(x) + 0.5, p.y);
      ctx.lineTo(Math.round(x) + 0.5, p.y + p.height);
    }
    for (const y of this.guides.y) {
      ctx.moveTo(p.x, Math.round(y) + 0.5);
      ctx.lineTo(p.x + p.width, Math.round(y) + 0.5);
    }
    ctx.stroke();
    ctx.setLineDash([]);
  }

  /** Test hook: where a handle of the one selected clip is, in page pixels. */
  handleAt(handle: Handle): Point | null {
    const [layer] = this.selectedLayers();
    if (!layer) return null;
    const found =
      handle === "move"
        ? { at: this.toScreen(layer.transform, 0, 0) }
        : this.handles(layer.transform).find((h) => h.handle === handle);
    if (!found?.at) return null;
    const r = this.canvas.getBoundingClientRect();
    return { x: r.left + found.at.x, y: r.top + found.at.y };
  }
}
