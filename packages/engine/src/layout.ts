// Pure sizing for the preview: no GPU, no DOM. Sizes are in device pixels.

export interface Size {
  width: number;
  height: number;
}

export interface Rect extends Size {
  x: number;
  y: number;
}

/** Preview resolution as a fraction of the sequence size (spec §3.4). */
export type PreviewScale = 1 | 0.5 | 0.25;

/** Rounds to the nearest even number, at least 2. Video sizes are even; odd sizes smear chroma. */
function even(n: number): number {
  return Math.max(2, 2 * Math.round(n / 2));
}

/** The size the compositor renders at: the sequence size times the preview scale. */
export function workingSize(sequence: Size, scale: PreviewScale): Size {
  return { width: even(sequence.width * scale), height: even(sequence.height * scale) };
}

/** Size of a picture after a clockwise rotation in whole quarter turns. */
export function rotatedSize(size: Size, rotation: number): Size {
  return rotation % 180 === 0 ? size : { width: size.height, height: size.width };
}

/**
 * The largest rectangle with the content's aspect ratio that fits in the viewport, centred, in
 * whole pixels. The rest of the viewport is letterbox.
 */
export function containRect(content: Size, viewport: Size): Rect {
  const scale = Math.min(viewport.width / content.width, viewport.height / content.height);
  const width = Math.max(1, Math.round(content.width * scale));
  const height = Math.max(1, Math.round(content.height * scale));
  return {
    x: Math.floor((viewport.width - width) / 2),
    y: Math.floor((viewport.height - height) / 2),
    width,
    height,
  };
}

/**
 * Texture-coordinate scale that fits content of one aspect ratio inside a target of another
 * ("contain"). Coordinates are scaled about the centre; anything outside 0..1 is letterbox.
 */
export function containUvScale(content: Size, target: Size): [number, number] {
  const contentAspect = content.width / content.height;
  const targetAspect = target.width / target.height;
  return contentAspect > targetAspect
    ? [1, contentAspect / targetAspect]
    : [targetAspect / contentAspect, 1];
}
