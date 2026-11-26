// Dragging media from the bin onto the timeline. While a drag is over a page, the browser hides
// its data until the drop, so the id being dragged is also kept here for the drop preview.

export const MEDIA_DRAG_TYPE = "application/x-renderly-media";

let dragged: string | null = null;

export function startMediaDrag(event: DragEvent, mediaId: string): void {
  event.dataTransfer?.setData(MEDIA_DRAG_TYPE, mediaId);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = "copy";
  dragged = mediaId;
  addEventListener("dragend", () => (dragged = null), { once: true });
}

/** The media id being dragged over the page, if the drag carries one. */
export function draggedMedia(event: DragEvent): string | null {
  if (!event.dataTransfer?.types.includes(MEDIA_DRAG_TYPE)) return null;
  return dragged;
}
