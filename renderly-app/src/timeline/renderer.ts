// Pure canvas draw code for the timeline. No hex color literals here — every color comes
// from timeline/theme.ts (which reads styles/tokens.css), enforced by a grep gate (see
// docs/architecture.md). No DOM/event handling here either — see interactions.ts.

import { fileName, formatTimecode } from "../lib/format";
import {
  clipDurationSecs,
  sequenceMediaName,
  type Project,
  type Selection,
  type Track,
} from "../lib/types";
import {
  clipLeft,
  CLIP_BOTTOM_PAD,
  CLIP_TOP_PAD,
  displayOrder,
  listTransitionJunctions,
  RULER_H,
  secsFromCanvasX,
  timelineDuration,
  trackLayout,
  TRACK_LABEL_W,
} from "./layout";
import { getTimelineTheme } from "./theme";
import type { DragGhost, MediaAssetEntry, ThumbnailAsset, WaveformAsset } from "../store/editorStore";

export interface TimelineRenderState {
  project: Project | null;
  playheadSecs: number;
  selection: Selection | null;
  /** C3: all selected clips (borders drawn for each). Falls back to `[selection]`. */
  selections?: Selection[];
  pxPerSec: number;
  scrollX?: number;
  scrollY?: number;
  dragGhost?: DragGhost | null;
  /** C4: highlighted junction while dragging a transition from the panel. */
  transitionDropTarget?: { trackId: string; clipId: string; valid: boolean } | null;
  snapGuideSecs?: number | null;
  mediaAssets?: Record<string, MediaAssetEntry>;
  /** Full clip ids an agent edit just touched; drawn with a short dashed outline. */
  highlightClipIds?: string[];
}

/// Tiles the strip image across the clip's rectangle, sampling the tile nearest each
/// on-screen column's source time — the same "filmstrip" look CapCut uses. Correctly
/// reflects trimming: `clip.source_in_secs`/`source_out_secs` (not the whole media) map
/// onto the drawn range, so a trimmed clip shows only its trimmed sub-range of tiles.
function drawVideoThumbnails(
  ctx: CanvasRenderingContext2D,
  sourceInSecs: number,
  sourceOutSecs: number,
  thumb: ThumbnailAsset,
  x: number,
  y: number,
  cw: number,
  ch: number,
) {
  if (!thumb.image || thumb.intervalSecs <= 0) return;
  const tileCount = thumb.cols * thumb.rows;
  const aspect = thumb.tileWidth / thumb.tileHeight;
  const drawH = ch;
  const drawW = Math.max(4, drawH * aspect);
  const duration = Math.max(sourceOutSecs - sourceInSecs, 1e-6);

  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, cw, ch);
  ctx.clip();
  for (let dx = 0; dx < cw; dx += drawW) {
    const sourceTime = sourceInSecs + (dx / cw) * duration;
    const tileIndex = Math.max(0, Math.min(tileCount - 1, Math.round(sourceTime / thumb.intervalSecs)));
    const tileX = (tileIndex % thumb.cols) * thumb.tileWidth;
    const tileY = Math.floor(tileIndex / thumb.cols) * thumb.tileHeight;
    ctx.drawImage(
      thumb.image,
      tileX,
      tileY,
      thumb.tileWidth,
      thumb.tileHeight,
      x + dx,
      y,
      Math.min(drawW, x + cw - (x + dx)),
      drawH,
    );
  }
  ctx.restore();
}

/// `peaks` covers the whole source media at a fixed `bucketSecs` stride regardless of
/// trim — slices out just the buckets in `[sourceInSecs, sourceOutSecs)` so a trimmed
/// clip's waveform matches what will actually play.
function drawWaveform(
  ctx: CanvasRenderingContext2D,
  sourceInSecs: number,
  sourceOutSecs: number,
  wave: WaveformAsset,
  x: number,
  y: number,
  cw: number,
  ch: number,
  color: string,
) {
  const { peaks, bucketSecs } = wave;
  if (peaks.length === 0 || bucketSecs <= 0) return;
  const startIdx = Math.max(0, Math.floor(sourceInSecs / bucketSecs));
  const endIdx = Math.min(peaks.length, Math.ceil(sourceOutSecs / bucketSecs));
  const count = endIdx - startIdx;
  if (count <= 0) return;

  const mid = y + ch / 2;
  const barW = Math.max(1, cw / count);
  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, cw, ch);
  ctx.clip();
  ctx.fillStyle = color;
  for (let i = 0; i < count; i++) {
    const peak = Math.min(1, peaks[startIdx + i] ?? 0);
    const barH = Math.max(1, peak * ch);
    ctx.fillRect(x + i * barW, mid - barH / 2, Math.max(1, barW - 0.5), barH);
  }
  ctx.restore();
}

function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  const rr = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + rr, y);
  ctx.arcTo(x + w, y, x + w, y + h, rr);
  ctx.arcTo(x + w, y + h, x, y + h, rr);
  ctx.arcTo(x, y + h, x, y, rr);
  ctx.arcTo(x, y, x + w, y, rr);
  ctx.closePath();
}

function clipColors(
  kind: Track["kind"],
  special?: "adjustment" | "sequence",
): { fill: string; edge: string } {
  const theme = getTimelineTheme();
  // Adjustment layers and nested sequences get their own tint so they never read as media.
  if (special === "adjustment") return { fill: theme.clipAdjustmentBg, edge: theme.clipAdjustmentEdge };
  if (special === "sequence") return { fill: theme.clipSequenceBg, edge: theme.clipSequenceEdge };
  if (kind === "video") return { fill: theme.clipVideoBg, edge: theme.clipVideoEdge };
  if (kind === "audio") return { fill: theme.clipAudioBg, edge: theme.clipAudioEdge };
  return { fill: theme.clipCaptionBg, edge: theme.clipCaptionEdge };
}

const CLIP_RADIUS = 6;

/// Name chip in the clip's top-left corner (approved redesign): translucent dark pill so
/// the label stays readable over filmstrip thumbnails in both themes.
function drawClipLabelChip(
  ctx: CanvasRenderingContext2D,
  label: string,
  x: number,
  cy: number,
  cw: number,
) {
  if (cw <= 40 || !label) return;
  const theme = getTimelineTheme();
  ctx.save();
  ctx.font = "600 10px var(--font-ui), Segoe UI, sans-serif";
  const maxW = cw * 0.7 - 12;
  let text = label;
  while (text.length > 1 && ctx.measureText(text).width > maxW) text = text.slice(0, -1);
  if (text !== label) text = `${text.slice(0, -1)}…`;
  const tw = ctx.measureText(text).width;
  ctx.fillStyle = theme.clipLabelBg;
  roundRect(ctx, x + 5, cy + 4, tw + 12, 15, 3);
  ctx.fill();
  ctx.fillStyle = theme.clipLabelText;
  ctx.fillText(text, x + 11, cy + 15);
  ctx.restore();
}

/// Trim grips on the selected clip: accent tabs with a dark notch, rounded on the outer
/// side to match the clip card's corner radius.
function drawTrimGrips(
  ctx: CanvasRenderingContext2D,
  x: number,
  cy: number,
  cw: number,
  ch: number,
) {
  const theme = getTimelineTheme();
  const gw = 8;
  ctx.save();
  roundRect(ctx, x, cy, cw, ch, CLIP_RADIUS);
  ctx.clip();
  ctx.fillStyle = theme.clipBorderSelected;
  ctx.fillRect(x, cy, gw, ch);
  ctx.fillRect(x + cw - gw, cy, gw, ch);
  ctx.fillStyle = theme.timelineBg;
  const notchH = Math.min(14, ch - 8);
  ctx.fillRect(x + gw / 2 - 1, cy + (ch - notchH) / 2, 2, notchH);
  ctx.fillRect(x + cw - gw / 2 - 1, cy + (ch - notchH) / 2, 2, notchH);
  ctx.restore();
}

/// Diagonal hatch overlay marking a locked track's lane — mouse edits are refused there
/// (see interactions.ts), but CLI/MCP agents can still edit it by design.
function drawLockedHatch(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number) {
  const theme = getTimelineTheme();
  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, w, h);
  ctx.clip();
  ctx.strokeStyle = theme.lockedHatch;
  ctx.lineWidth = 1;
  const step = 10;
  for (let sx = -h; sx < w; sx += step) {
    ctx.beginPath();
    ctx.moveTo(x + sx, y + h);
    ctx.lineTo(x + sx + h, y);
    ctx.stroke();
  }
  ctx.restore();
}

/// C4: chevron badge + overlap tint at clip junctions where an outgoing transition applies.
function drawTransitionJunctions(
  ctx: CanvasRenderingContext2D,
  project: Project,
  pxPerSec: number,
  canvasW: number,
  canvasH: number,
  scrollX: number,
  scrollY: number,
  dropTarget: { trackId: string; clipId: string; valid: boolean } | null | undefined,
) {
  const theme = getTimelineTheme();
  const junctions = listTransitionJunctions(project, canvasH, pxPerSec, scrollX, scrollY);

  for (const j of junctions) {
    if (j.junctionX < TRACK_LABEL_W - 8 || j.junctionX > canvasW + 8) continue;

    const isTarget =
      dropTarget?.trackId === j.trackId && dropTarget.clipId === j.outgoingClipId;
    const targetColor = dropTarget?.valid ? theme.accent : theme.danger;

    if (j.overlapPx > 0) {
      const ox = j.junctionX - j.overlapPx;
      ctx.save();
      ctx.globalAlpha = 0.22;
      ctx.fillStyle = theme.accent;
      ctx.fillRect(ox, j.bodyY, j.overlapPx, j.bodyH);
      ctx.fillRect(j.junctionX, j.bodyY, j.overlapPx, j.bodyH);
      ctx.globalAlpha = 1;
      ctx.restore();
    }

    ctx.save();
    if (isTarget) {
      ctx.strokeStyle = targetColor;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.arc(j.junctionX, j.bodyY + j.bodyH / 2, 13, 0, Math.PI * 2);
      ctx.stroke();
    }

    // Diamond badge centered on the junction (approved redesign): raised square rotated
    // 45°, accent-bordered; half-filled when a transition is applied, hollow otherwise.
    const cx = j.junctionX;
    const cy = j.bodyY + j.bodyH / 2;
    const r = 7;
    ctx.translate(cx, cy);
    ctx.rotate(Math.PI / 4);
    ctx.fillStyle = theme.trackHeaderBg;
    roundRect(ctx, -r, -r, r * 2, r * 2, 3);
    ctx.fill();
    ctx.strokeStyle = j.transition ? theme.accent : theme.text3;
    ctx.lineWidth = 1.5;
    ctx.globalAlpha = j.transition ? 1 : 0.65;
    roundRect(ctx, -r, -r, r * 2, r * 2, 3);
    ctx.stroke();
    if (j.transition) {
      ctx.globalAlpha = 0.7;
      ctx.fillStyle = theme.accent;
      ctx.beginPath();
      ctx.moveTo(-r + 3, -r + 3);
      ctx.lineTo(r - 3, -r + 3);
      ctx.lineTo(-r + 3, r - 3);
      ctx.closePath();
      ctx.fill();
    }
    ctx.restore();
  }
}

function drawDragGhost(
  ctx: CanvasRenderingContext2D,
  project: Project,
  ghost: DragGhost,
  pxPerSec: number,
  canvasW: number,
  canvasH: number,
  scrollX = 0,
  scrollY = 0,
) {
  const theme = getTimelineTheme();
  const x = clipLeft(ghost.positionSecs, pxPerSec, scrollX);
  const cw = Math.max(ghost.durationSecs * pxPerSec, 8);
  const color = ghost.valid ? theme.accent : theme.danger;

  const rowCount = displayOrder(project).length;
  const { trackH, laneTop } = trackLayout(canvasH, Math.max(rowCount, 1), scrollY);
  const isNewTrack = ghost.trackIndex == null;
  const y = isNewTrack ? laneTop(ghost.row) : laneTop(ghost.row) + CLIP_TOP_PAD;
  const h = isNewTrack ? trackH : trackH - CLIP_TOP_PAD - CLIP_BOTTOM_PAD;

  if (isNewTrack) {
    // New-track insertion line sits at the lane's near edge: above row 0 (drop-above-top,
    // `ghost.row === -1`) draws at the lane's bottom edge; below the last row draws at its
    // top edge — either way, right where the new lane will appear.
    const lineY = ghost.row < 0 ? y + trackH : y;
    ctx.save();
    ctx.strokeStyle = color;
    ctx.setLineDash([6, 4]);
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(TRACK_LABEL_W, lineY);
    ctx.lineTo(canvasW, lineY);
    ctx.stroke();
    ctx.restore();
  }

  ctx.save();
  ctx.globalAlpha = 0.5;
  ctx.fillStyle = color;
  roundRect(ctx, x, y, cw, h, 5);
  ctx.fill();
  ctx.globalAlpha = 1;
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  roundRect(ctx, x, y, cw, h, 5);
  ctx.stroke();
  ctx.restore();
}

/// Resizes the backing bitmap only when the CSS size actually changed — assigning
/// `canvas.width`/`.height` (even to their current value) forces the browser to discard
/// and reallocate the bitmap, which is wasteful when this runs ~30x/sec during playback
/// (the playhead used to live on this same canvas — see `renderTimelineOverlay` below for
/// where it moved). Always re-applies the DPR transform, which is cheap and must track
/// the caller's current `dpr` even when the size didn't change.
function syncCanvasSize(
  canvas: HTMLCanvasElement,
  ctx: CanvasRenderingContext2D,
  dpr: number,
): { w: number; h: number } {
  const rect = canvas.getBoundingClientRect();
  const targetW = Math.round(rect.width * dpr);
  const targetH = Math.round(rect.height * dpr);
  if (canvas.width !== targetW || canvas.height !== targetH) {
    canvas.width = targetW;
    canvas.height = targetH;
  }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { w: rect.width, h: rect.height };
}

export interface TimelineOverlayState {
  playheadSecs: number;
  pxPerSec: number;
  scrollX?: number;
  snapGuideSecs?: number | null;
  /// Suppress drawing entirely when there's no project/timeline (matches the "Import
  /// media…" empty state the main canvas shows instead of a grid).
  hasProject: boolean;
}

/// Playhead + snap guide, previously drawn inline in `renderTimeline` at the end of every
/// frame — moved to a separate, transparent, pointer-events:none canvas stacked on top
/// (see `TimelineCanvas.tsx`) so scrubbing/playback (which move only these two things,
/// ~30x/sec) no longer force a full repaint of every clip/thumbnail/waveform underneath.
export function renderTimelineOverlay(canvas: HTMLCanvasElement, state: TimelineOverlayState): void {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const dpr = window.devicePixelRatio || 1;
  const { w, h } = syncCanvasSize(canvas, ctx, dpr);
  ctx.clearRect(0, 0, w, h);
  if (!state.hasProject) return;

  const theme = getTimelineTheme();
  const { playheadSecs, pxPerSec, snapGuideSecs, scrollX = 0 } = state;

  if (snapGuideSecs != null) {
    const gx = clipLeft(snapGuideSecs, pxPerSec, scrollX);
    if (gx >= TRACK_LABEL_W && gx <= w) {
      ctx.save();
      ctx.strokeStyle = theme.snapGuide;
      ctx.setLineDash([4, 3]);
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(gx + 0.5, RULER_H);
      ctx.lineTo(gx + 0.5, h);
      ctx.stroke();
      ctx.restore();
    }
  }

  // Playhead — polished marker with ruler head + shadow line
  const phx = clipLeft(playheadSecs, pxPerSec, scrollX);
  if (phx >= TRACK_LABEL_W - 8 && phx <= w + 8) {
    ctx.save();
    ctx.shadowColor = theme.clipShadow;
    ctx.shadowBlur = 4;
    ctx.strokeStyle = theme.playhead;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(phx + 0.5, RULER_H - 2);
    ctx.lineTo(phx + 0.5, h);
    ctx.stroke();
    ctx.shadowBlur = 0;

    // Capsule head on the ruler
    const headW = 10;
    const headH = 14;
    ctx.fillStyle = theme.playhead;
    roundRect(ctx, phx - headW / 2, 4, headW, headH, 2);
    ctx.fill();
    // Accent notch
    ctx.fillStyle = theme.accent;
    ctx.fillRect(phx - 1, 6, 2, headH - 4);
    ctx.restore();
  }
}

export function renderTimeline(canvas: HTMLCanvasElement, state: TimelineRenderState): void {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const theme = getTimelineTheme();

  const dpr = window.devicePixelRatio || 1;
  const { w, h } = syncCanvasSize(canvas, ctx, dpr);

  ctx.fillStyle = theme.timelineBg;
  ctx.fillRect(0, 0, w, h);

  const { project, selection, pxPerSec, dragGhost, scrollX = 0, scrollY = 0 } = state;
  const selections =
    state.selections && state.selections.length > 0
      ? state.selections
      : selection
        ? [selection]
        : [];
  const isSelected = (trackId: string, clipId: string) =>
    selections.some((s) => s.trackId === trackId && s.clipId === clipId);

  if (!project || project.tracks.length === 0) {
    ctx.fillStyle = theme.text3;
    ctx.font = "500 13px var(--font-ui), Segoe UI, sans-serif";
    ctx.textAlign = "center";
    ctx.fillText("Import media to build your timeline", w / 2, h / 2);
    ctx.textAlign = "left";
    return;
  }

  const duration = timelineDuration(project);
  const order = displayOrder(project);
  const { trackH, laneTop } = trackLayout(h, order.length, scrollY);

  // Lane background (scrollable content region)
  ctx.fillStyle = theme.timelineBg;
  ctx.fillRect(TRACK_LABEL_W, RULER_H, w - TRACK_LABEL_W, h - RULER_H);

  // Ruler (fixed height, scrolls horizontally with content)
  ctx.fillStyle = theme.rulerBg;
  ctx.fillRect(0, 0, w, RULER_H);
  ctx.strokeStyle = theme.border;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(0, RULER_H + 0.5);
  ctx.lineTo(w, RULER_H + 0.5);
  ctx.stroke();

  ctx.fillStyle = theme.rulerText;
  ctx.font = "500 10px var(--font-ui), Segoe UI, sans-serif";
  const step = pxPerSec >= 120 ? 0.5 : pxPerSec >= 60 ? 1 : pxPerSec >= 30 ? 2 : 5;
  const t0 = Math.max(0, Math.floor(secsFromCanvasX(TRACK_LABEL_W, pxPerSec, scrollX) / step) * step);
  const t1 = secsFromCanvasX(w + 40, pxPerSec, scrollX);
  for (let t = t0; t <= Math.max(duration, t1); t += step) {
    const x = clipLeft(t, pxPerSec, scrollX);
    if (x < TRACK_LABEL_W - 2 || x > w + 2) continue;
    const major = Math.abs(t % (step >= 1 ? 5 : 1)) < 1e-6 || Math.abs(t) < 1e-9;
    // Approved redesign: lanes stay quiet — only major ticks continue below the ruler,
    // and only as a barely-there stripe. Minor ticks live on the ruler alone.
    if (major) {
      ctx.strokeStyle = theme.laneStripe;
      ctx.beginPath();
      ctx.moveTo(x + 0.5, RULER_H);
      ctx.lineTo(x + 0.5, h);
      ctx.stroke();
    }
    ctx.strokeStyle = major ? theme.rulerLineStrong : theme.rulerLineWeak;
    ctx.beginPath();
    ctx.moveTo(x + 0.5, RULER_H - (major ? 10 : 5));
    ctx.lineTo(x + 0.5, RULER_H);
    ctx.stroke();
    if (major) {
      ctx.fillStyle = theme.rulerText;
      ctx.font = "500 10px Consolas, var(--font-ui), monospace";
      ctx.fillText(formatTimecode(t).slice(0, 8), x + 4, 12);
    }
  }

  // Track labels/mute/lock/hide/delete are a real DOM column (components/timeline/
  // TrackHeaders.tsx), overlaid on top of the TRACK_LABEL_W gutter reserved here — not
  // drawn on canvas, so they can be actual interactive controls (dbl-click rename,
  // toggle buttons) instead of hit-tested canvas regions.
  order.forEach(({ track }, i) => {
    const y = laneTop(i);
    if (y + trackH < RULER_H || y > h) return;

    ctx.fillStyle = theme.trackLaneBg;
    ctx.fillRect(TRACK_LABEL_W, y - 2, w - TRACK_LABEL_W, trackH + 4);
    // Lane separator (approved redesign: rows read as rows).
    ctx.strokeStyle = theme.border;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(TRACK_LABEL_W, y + trackH + 2.5);
    ctx.lineTo(w, y + trackH + 2.5);
    ctx.stroke();

    for (const clip of track.clips) {
      const x = clipLeft(clip.position_secs, pxPerSec, scrollX);
      const cw = Math.max(clipDurationSecs(clip) * pxPerSec, 8);
      if (x + cw < TRACK_LABEL_W || x > w) continue;
      const cy = y + CLIP_TOP_PAD;
      const ch = trackH - CLIP_TOP_PAD - CLIP_BOTTOM_PAD;
      const selected = isSelected(track.id, clip.id);
      const disabled = clip.type !== "caption" && !clip.enabled;
      const clipMedia =
        clip.type === "video" || clip.type === "audio"
          ? project.media.find((m) => m.id === clip.media_id)
          : undefined;
      const colors = clipColors(
        track.kind,
        clip.type === "adjustment" ? "adjustment" : clipMedia?.kind === "sequence" ? "sequence" : undefined,
      );

      // Card: soft drop shadow, kind-colored fill, 1px edge stroke.
      ctx.save();
      ctx.globalAlpha = disabled ? 0.4 : 1;
      ctx.shadowColor = theme.clipShadow;
      ctx.shadowBlur = 6;
      ctx.shadowOffsetY = 2;
      ctx.fillStyle = colors.fill;
      roundRect(ctx, x, cy, cw, ch, CLIP_RADIUS);
      ctx.fill();
      ctx.shadowBlur = 0;
      ctx.shadowOffsetY = 0;

      if (clip.type === "video") {
        const thumb = state.mediaAssets?.[clip.media_id]?.thumbnails;
        if (thumb) {
          ctx.save();
          roundRect(ctx, x, cy, cw, ch, CLIP_RADIUS);
          ctx.clip();
          drawVideoThumbnails(ctx, clip.source_in_secs, clip.source_out_secs, thumb, x, cy, cw, ch);
          ctx.restore();
        }
      } else if (clip.type === "audio") {
        const waveform = state.mediaAssets?.[clip.media_id]?.waveform;
        if (waveform) {
          ctx.save();
          roundRect(ctx, x, cy, cw, ch, CLIP_RADIUS);
          ctx.clip();
          drawWaveform(
            ctx,
            clip.source_in_secs,
            clip.source_out_secs,
            waveform,
            x,
            cy + 3,
            cw,
            ch - 6,
            theme.clipAudioWave,
          );
          ctx.restore();
        }
      }

      ctx.strokeStyle = colors.edge;
      ctx.lineWidth = 1;
      roundRect(ctx, x + 0.5, cy + 0.5, cw - 1, ch - 1, CLIP_RADIUS);
      ctx.stroke();
      ctx.restore();

      if (clip.type === "caption") {
        // Caption pill: text vertically centered, no chip.
        ctx.save();
        roundRect(ctx, x, cy, cw, ch, CLIP_RADIUS);
        ctx.clip();
        ctx.fillStyle = theme.clipLabelText;
        ctx.font = "500 10.5px var(--font-ui), Segoe UI, sans-serif";
        if (cw > 24) ctx.fillText(clip.text.slice(0, Math.floor(cw / 6)), x + 8, cy + ch / 2 + 3.5);
        ctx.restore();
      } else {
        const media = clipMedia;
        // Adjustment layers are a tinted, labelled block; a nested sequence shows its name
        // (double-click opens it).
        const label =
          clip.type === "adjustment"
            ? "Adjustment"
            : media
              ? (sequenceMediaName(project, media) ?? fileName(media.path))
              : fileName(track.kind);
        drawClipLabelChip(ctx, label, x, cy, cw);
        // Duration, bottom-right, monospace.
        if (cw > 64) {
          ctx.save();
          ctx.font = "500 9px Consolas, monospace";
          ctx.fillStyle = theme.clipLabelText;
          ctx.globalAlpha = 0.75;
          const dur = `${clipDurationSecs(clip).toFixed(1)}s`;
          ctx.fillText(dur, x + cw - ctx.measureText(dur).width - 6, cy + ch - 5);
          ctx.restore();
        }
      }

      if (selected) {
        // Accent border + soft glow, then trim grips.
        ctx.save();
        ctx.strokeStyle = theme.clipBorderSelected;
        ctx.globalAlpha = 0.22;
        ctx.lineWidth = 5;
        roundRect(ctx, x - 1, cy - 1, cw + 2, ch + 2, CLIP_RADIUS + 1);
        ctx.stroke();
        ctx.globalAlpha = 1;
        ctx.lineWidth = 1.5;
        roundRect(ctx, x, cy, cw, ch, CLIP_RADIUS);
        ctx.stroke();
        ctx.restore();
        if (cw > 30) drawTrimGrips(ctx, x, cy, cw, ch);
      }

      if (state.highlightClipIds?.includes(clip.id)) {
        // Agent edit highlight: a dashed accent outline the store clears after a moment.
        ctx.save();
        ctx.strokeStyle = theme.clipBorderSelected;
        ctx.lineWidth = 2;
        ctx.setLineDash([5, 3]);
        roundRect(ctx, x - 3, cy - 3, cw + 6, ch + 6, CLIP_RADIUS + 3);
        ctx.stroke();
        ctx.restore();
      }
    }

    if (track.locked) {
      drawLockedHatch(ctx, TRACK_LABEL_W, y, w - TRACK_LABEL_W, trackH);
    }
  });

  drawTransitionJunctions(
    ctx,
    project,
    pxPerSec,
    w,
    h,
    scrollX,
    scrollY,
    state.transitionDropTarget,
  );

  if (dragGhost) {
    drawDragGhost(ctx, project, dragGhost, pxPerSec, w, h, scrollX, scrollY);
  }
  // Playhead + snap guide are drawn on a separate overlay canvas — see
  // `renderTimelineOverlay` — so their ~30Hz motion during playback/scrub doesn't repaint
  // this canvas's clips/thumbnails/waveforms.

  // Fixed gutter over label column (so grid lines don't show under headers)
  ctx.fillStyle = theme.trackHeaderBg;
  ctx.fillRect(0, RULER_H, TRACK_LABEL_W, h - RULER_H);
  ctx.strokeStyle = theme.border;
  ctx.beginPath();
  ctx.moveTo(TRACK_LABEL_W + 0.5, RULER_H);
  ctx.lineTo(TRACK_LABEL_W + 0.5, h);
  ctx.stroke();
}
