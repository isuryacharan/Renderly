// Reads CSS custom properties from styles/tokens.css once and caches them, so canvas draw
// code (renderer.ts) never hardcodes a color literal — grep-gated in CI/review (see
// docs/architecture.md). Call `getTimelineTheme()` lazily; it self-initializes on first use
// and re-reads if the cache hasn't been built yet (cheap — one getComputedStyle call).

export interface TimelineTheme {
  timelineBg: string;
  rulerBg: string;
  rulerText: string;
  rulerLineStrong: string;
  rulerLineWeak: string;
  trackHeaderBg: string;
  trackLaneBg: string;
  text1: string;
  text2: string;
  text3: string;
  clipVideoBg: string;
  clipVideoEdge: string;
  clipAudioBg: string;
  clipAudioEdge: string;
  clipAudioWave: string;
  clipCaptionBg: string;
  clipCaptionEdge: string;
  clipAdjustmentBg: string;
  clipAdjustmentEdge: string;
  clipSequenceBg: string;
  clipSequenceEdge: string;
  clipLabelBg: string;
  clipLabelText: string;
  laneStripe: string;
  clipBorderSelected: string;
  clipShadow: string;
  playhead: string;
  snapGuide: string;
  accent: string;
  border: string;
  danger: string;
  lockedHatch: string;
}

let cached: TimelineTheme | null = null;

function cssVar(styles: CSSStyleDeclaration, name: string): string {
  return styles.getPropertyValue(name).trim();
}

export function getTimelineTheme(): TimelineTheme {
  if (cached) return cached;
  const styles = getComputedStyle(document.documentElement);
  cached = {
    timelineBg: cssVar(styles, "--timeline-bg"),
    rulerBg: cssVar(styles, "--ruler-bg"),
    rulerText: cssVar(styles, "--ruler-text"),
    rulerLineStrong: cssVar(styles, "--ruler-line-strong"),
    rulerLineWeak: cssVar(styles, "--ruler-line-weak"),
    trackHeaderBg: cssVar(styles, "--track-header-bg"),
    trackLaneBg: cssVar(styles, "--track-lane-bg"),
    text1: cssVar(styles, "--text-1"),
    text2: cssVar(styles, "--text-2"),
    text3: cssVar(styles, "--text-3"),
    clipVideoBg: cssVar(styles, "--clip-video-bg"),
    clipVideoEdge: cssVar(styles, "--clip-video-edge"),
    clipAudioBg: cssVar(styles, "--clip-audio-bg"),
    clipAudioEdge: cssVar(styles, "--clip-audio-edge"),
    clipAudioWave: cssVar(styles, "--clip-audio-wave"),
    clipCaptionBg: cssVar(styles, "--clip-caption-bg"),
    clipCaptionEdge: cssVar(styles, "--clip-caption-edge"),
    clipAdjustmentBg: cssVar(styles, "--clip-adjustment-bg"),
    clipAdjustmentEdge: cssVar(styles, "--clip-adjustment-edge"),
    clipSequenceBg: cssVar(styles, "--clip-sequence-bg"),
    clipSequenceEdge: cssVar(styles, "--clip-sequence-edge"),
    clipLabelBg: cssVar(styles, "--clip-label-bg"),
    clipLabelText: cssVar(styles, "--clip-label-text"),
    laneStripe: cssVar(styles, "--lane-stripe"),
    clipBorderSelected: cssVar(styles, "--clip-border-selected"),
    clipShadow: cssVar(styles, "--clip-shadow"),
    playhead: cssVar(styles, "--playhead"),
    snapGuide: cssVar(styles, "--snap-guide"),
    accent: cssVar(styles, "--accent"),
    border: cssVar(styles, "--border"),
    danger: cssVar(styles, "--danger"),
    lockedHatch: cssVar(styles, "--locked-hatch"),
  };
  return cached;
}

/// Test-only / hot-reload escape hatch: forces the next `getTimelineTheme()` call to
/// re-read the DOM (tokens.css is not expected to change at runtime otherwise).
export function resetTimelineThemeCache(): void {
  cached = null;
}
