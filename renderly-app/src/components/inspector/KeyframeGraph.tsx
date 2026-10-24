import { useMemo, useRef, useState } from "react";
import type { Keyframe } from "../../lib/types";

/** A key as the graph draws it: one scalar value (array-valued keys show their first component). */
export type GraphKey = Omit<Keyframe, "value"> & { value: number };

interface Props {
  track: { keys: GraphKey[] };
  duration: number;
  width: number;
  height: number;
  onUpdateKey: (idx: number, patch: Partial<Keyframe>) => void;
}

export function KeyframeGraph({ track, duration, width, height, onUpdateKey }: Props) {
  const containerRef = useRef<SVGSVGElement>(null);
  const [dragging, setDragging] = useState<{ idx: number; type: "key" | "hl" | "hr" } | null>(null);

  const padding = 20;
  const graphWidth = width - padding * 2;
  const graphHeight = height - padding * 2;

  const { minVal, maxVal } = useMemo(() => {
    if (track.keys.length === 0) return { minVal: 0, maxVal: 1 };
    let min = Math.min(...track.keys.map((k) => k.value));
    let max = Math.max(...track.keys.map((k) => k.value));
    if (max - min < 0.1) {
      min -= 0.5;
      max += 0.5;
    }
    const margin = (max - min) * 0.2;
    return { minVal: min - margin, maxVal: max + margin };
  }, [track.keys]);

  const toX = (t: number) => padding + (duration > 0 ? (t / duration) * graphWidth : 0);
  const toY = (v: number) => padding + graphHeight - ((v - minVal) / (maxVal - minVal)) * graphHeight;

  // Times are integer µs relative to the clip start; round pointer positions.
  const fromX = (x: number) => Math.round(((x - padding) / graphWidth) * duration);
  const fromY = (y: number) => minVal + (1.0 - (y - padding) / graphHeight) * (maxVal - minVal);

  const handlePointerMove = (e: React.PointerEvent) => {
    if (!dragging || !containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;

    const key = track.keys[dragging.idx];
    if (dragging.type === "key") {
      const time_us = Math.max(0, Math.min(duration, fromX(x)));
      const value = fromY(y);
      onUpdateKey(dragging.idx, { time_us, value });
    } else if (dragging.type === "hl") {
      const dt = fromX(x) - key.time_us;
      const dv = fromY(y) - key.value;
      onUpdateKey(dragging.idx, { handle_left: [dt, dv] });
    } else if (dragging.type === "hr") {
      const dt = fromX(x) - key.time_us;
      const dv = fromY(y) - key.value;
      onUpdateKey(dragging.idx, { handle_right: [dt, dv] });
    }
  };

  const pathData = useMemo(() => {
    if (track.keys.length < 2) return "";
    let d = `M ${toX(track.keys[0].time_us)} ${toY(track.keys[0].value)}`;
    for (let i = 0; i < track.keys.length - 1; i++) {
      const a = track.keys[i];
      const b = track.keys[i + 1];
      if (a.easing === "bezier") {
        const p3x = toX(b.time_us);
        const p3y = toY(b.value);

        const hr = a.handle_right ?? [Math.round((b.time_us - a.time_us) * 0.33), 0];
        const hl = b.handle_left ?? [-Math.round((b.time_us - a.time_us) * 0.33), 0];

        const p1x = toX(a.time_us + hr[0]);
        const p1y = toY(a.value + hr[1]);
        const p2x = toX(b.time_us + hl[0]);
        const p2y = toY(b.value + hl[1]);

        d += ` C ${p1x} ${p1y}, ${p2x} ${p2y}, ${p3x} ${p3y}`;
      } else {
        d += ` L ${toX(b.time_us)} ${toY(b.value)}`;
      }
    }
    return d;
  }, [track.keys, duration, width, height, minVal, maxVal]);

  return (
    <svg
      ref={containerRef}
      className="keyframe-graph"
      width={width}
      height={height}
      onPointerMove={handlePointerMove}
      onPointerUp={() => setDragging(null)}
      onPointerLeave={() => setDragging(null)}
    >
      <rect width={width} height={height} fill="var(--bg-3)" rx={4} />
      
      {/* Grid lines */}
      {[0, 0.25, 0.5, 0.75, 1].map((p) => (
        <line
          key={`v-${p}`}
          x1={padding + p * graphWidth}
          y1={padding}
          x2={padding + p * graphWidth}
          y2={padding + graphHeight}
          stroke="var(--bg-1)"
          strokeWidth={1}
          strokeDasharray="4 4"
        />
      ))}

      <path d={pathData} fill="none" stroke="var(--accent)" strokeWidth={2} />

      {track.keys.map((k, i) => {
        const kx = toX(k.time_us);
        const ky = toY(k.value);

        return (
          <g key={i}>
            {k.easing === "bezier" && k.handle_right && (
              <>
                <line
                  x1={kx}
                  y1={ky}
                  x2={toX(k.time_us + k.handle_right[0])}
                  y2={toY(k.value + k.handle_right[1])}
                  stroke="var(--text-3)"
                  strokeWidth={1}
                />
                <circle
                  cx={toX(k.time_us + k.handle_right[0])}
                  cy={toY(k.value + k.handle_right[1])}
                  r={4}
                  fill="var(--text-2)"
                  className="handle"
                  onPointerDown={(e) => {
                    e.stopPropagation();
                    setDragging({ idx: i, type: "hr" });
                  }}
                />
              </>
            )}
            {i > 0 && track.keys[i-1].easing === "bezier" && k.handle_left && (
              <>
                <line
                  x1={kx}
                  y1={ky}
                  x2={toX(k.time_us + k.handle_left[0])}
                  y2={toY(k.value + k.handle_left[1])}
                  stroke="var(--text-3)"
                  strokeWidth={1}
                />
                <circle
                  cx={toX(k.time_us + k.handle_left[0])}
                  cy={toY(k.value + k.handle_left[1])}
                  r={4}
                  fill="var(--text-2)"
                  className="handle"
                  onPointerDown={(e) => {
                    e.stopPropagation();
                    setDragging({ idx: i, type: "hl" });
                  }}
                />
              </>
            )}
            <circle
              cx={kx}
              cy={ky}
              r={6}
              fill={dragging?.idx === i && dragging.type === "key" ? "var(--accent)" : "var(--text-1)"}
              className="key-node"
              onPointerDown={(e) => {
                e.stopPropagation();
                setDragging({ idx: i, type: "key" });
              }}
            />
          </g>
        );
      })}
    </svg>
  );
}
