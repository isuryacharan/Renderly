// Drag-and-drop payload for applying a transition from TransitionsPanel onto a timeline
// junction (improvement-plan C4).

export const TRANSITION_DRAG_MIME = "application/x-renderly-transition";

export interface TransitionDragPayload {
  /** The transition item's ref (`builtin:crossfade@1`, `pack:<pack>/<item>@<v>`). */
  ref: string;
  duration_us: number;
}

export function startTransitionDrag(e: React.DragEvent, ref: string, durationUs: number) {
  const payload: TransitionDragPayload = { ref, duration_us: durationUs };
  e.dataTransfer.setData(TRANSITION_DRAG_MIME, JSON.stringify(payload));
  e.dataTransfer.effectAllowed = "copy";
}

export function readTransitionDrag(e: React.DragEvent | DragEvent): TransitionDragPayload | null {
  const raw = e.dataTransfer?.getData(TRANSITION_DRAG_MIME);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as Partial<TransitionDragPayload>;
    return typeof parsed.ref === "string" && typeof parsed.duration_us === "number"
      ? { ref: parsed.ref, duration_us: parsed.duration_us }
      : null;
  } catch {
    return null;
  }
}
