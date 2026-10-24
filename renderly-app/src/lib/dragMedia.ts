// Shared HTML5 drag-and-drop payload for dragging a media-bin card onto the timeline.
// Custom MIME type keeps this out of the OS-file-drop handling (`tauri://drag-drop`).

import type { MediaKind } from "./types";

export const MEDIA_DRAG_MIME = "application/x-renderly-media";

export interface MediaDragPayload {
  mediaId: string;
  kind: MediaKind;
  durationUs: number;
}

export function startMediaDrag(
  e: React.DragEvent,
  mediaId: string,
  kind: MediaKind,
  durationUs: number,
) {
  const payload: MediaDragPayload = { mediaId, kind, durationUs };
  e.dataTransfer.setData(MEDIA_DRAG_MIME, JSON.stringify(payload));
  e.dataTransfer.effectAllowed = "copy";
}

export function readMediaDrag(e: React.DragEvent | DragEvent): MediaDragPayload | null {
  const raw = e.dataTransfer?.getData(MEDIA_DRAG_MIME);
  if (!raw) return null;
  try {
    return JSON.parse(raw) as MediaDragPayload;
  } catch {
    return null;
  }
}
