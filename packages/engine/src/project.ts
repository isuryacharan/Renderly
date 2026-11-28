// From a project's evaluated sequence (core `evaluateSequence`) to the engine's edit list. The
// editor and the benchmark both go through here, so the benchmark measures what the editor plays.

import type { SequenceGraph } from "@renderly/core";
import type { Cut } from "./timeline";

/**
 * One cut per clip on every drawing layer; layer numbers are the graph's (0 is the bottom), and
 * layer 0's sound is muted when its track is. `source` gives a media item's index in the engine's
 * source list, or null to leave its clips out (the file cannot be read).
 */
export function cutsFromGraph(
  graph: SequenceGraph,
  source: (media: string) => number | null,
): Cut[] {
  const cuts: Cut[] = [];
  graph.layers.forEach((layer, index) => {
    for (const clip of layer.clips) {
      const i = source(clip.media);
      if (i === null) continue;
      cuts.push({
        source: i,
        in: clip.in,
        out: clip.out,
        start: clip.start,
        layer: index,
        transform: clip.transform,
        // A freeze frame is silent.
        muted: layer.muted || clip.freeze === true,
        ...(clip.keys ? { keys: clip.keys } : {}),
        ...(clip.freeze ? { freeze: true } : {}),
      });
    }
  });
  return cuts;
}
