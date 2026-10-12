import { useEffect, useRef, useState } from "react";
import { useEditorStore } from "../../store/editorStore";
import { selectActiveView } from "../../lib/container";
import { maxScrollX, maxScrollY, RULER_H, TRACK_LABEL_W } from "../../timeline/layout";

const BAR = 10; // scrollbar thickness in px

/// Custom timeline scrollbars (issue 8): the timeline is a manually-panned canvas, so it
/// has no native scrollbars. These DOM overlays reflect scrollX/scrollY vs. content size
/// and are draggable. Shown only when content overflows the viewport on that axis.
export function TimelineScrollbars({
  containerRef,
}: {
  containerRef: React.RefObject<HTMLDivElement | null>;
}) {
  const project = useEditorStore(selectActiveView);
  const pxPerSec = useEditorStore((s) => s.pxPerSec);
  const scrollX = useEditorStore((s) => s.scrollX);
  const scrollY = useEditorStore((s) => s.scrollY);
  const setScroll = useEditorStore((s) => s.setScroll);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const drag = useRef<{ axis: "x" | "y"; startPos: number; startScroll: number; ratio: number } | null>(
    null,
  );

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const measure = () => setSize({ w: el.clientWidth, h: el.clientHeight });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [containerRef]);

  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const d = drag.current;
      if (!d) return;
      const pos = d.axis === "x" ? e.clientX : e.clientY;
      const deltaScroll = (pos - d.startPos) * d.ratio;
      if (d.axis === "x") setScroll(d.startScroll + deltaScroll, scrollY);
      else setScroll(scrollX, d.startScroll + deltaScroll);
    };
    const onUp = () => {
      drag.current = null;
      document.body.style.userSelect = "";
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
  }, [scrollX, scrollY, setScroll]);

  if (!project || project.tracks.length === 0) return null;

  const viewW = Math.max(0, size.w - TRACK_LABEL_W);
  const viewH = Math.max(0, size.h - RULER_H);
  const maxX = maxScrollX(project, pxPerSec, size.w);
  const maxY = maxScrollY(project.tracks.length, size.h);

  // thumbLen / trackLen = viewport / content; content = viewport + maxScroll.
  const hThumb = maxX > 0 ? Math.max(28, (viewW * viewW) / (viewW + maxX)) : 0;
  const vThumb = maxY > 0 ? Math.max(28, (viewH * viewH) / (viewH + maxY)) : 0;
  const hTravel = viewW - hThumb;
  const vTravel = viewH - vThumb;
  const hPos = maxX > 0 ? (scrollX / maxX) * hTravel : 0;
  const vPos = maxY > 0 ? (scrollY / maxY) * vTravel : 0;

  const startDrag = (axis: "x" | "y", e: React.PointerEvent) => {
    e.preventDefault();
    const travel = axis === "x" ? hTravel : vTravel;
    const max = axis === "x" ? maxX : maxY;
    if (travel <= 0 || max <= 0) return;
    drag.current = {
      axis,
      startPos: axis === "x" ? e.clientX : e.clientY,
      startScroll: axis === "x" ? scrollX : scrollY,
      ratio: max / travel,
    };
    document.body.style.userSelect = "none";
  };

  return (
    <>
      {maxX > 0 && (
        <div className="tl-scrollbar horizontal" style={{ left: TRACK_LABEL_W, height: BAR }}>
          <div
            className="tl-scroll-thumb"
            style={{ width: hThumb, transform: `translateX(${hPos}px)` }}
            onPointerDown={(e) => startDrag("x", e)}
          />
        </div>
      )}
      {maxY > 0 && (
        <div className="tl-scrollbar vertical" style={{ top: RULER_H, width: BAR }}>
          <div
            className="tl-scroll-thumb"
            style={{ height: vThumb, transform: `translateY(${vPos}px)` }}
            onPointerDown={(e) => startDrag("y", e)}
          />
        </div>
      )}
    </>
  );
}
