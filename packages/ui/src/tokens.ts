// Renderly design tokens — generated from tokens/tokens.json. Do not edit.
// Use these resolved values where CSS variables cannot reach: canvas drawing for the
// timeline, scopes and waveforms. Everywhere else, use the CSS variables in tokens.css.

export type ThemeId = "dark" | "light";

export const themeIds: readonly ThemeId[] = ["dark","light"];

export const colors = {
  dark: {
    viewer: "#0e0e0f",
    bg: "#161617",
    surface: "#1c1c1e",
    surfaceRaised: "#262628",
    surfaceHover: "#2f2f32",
    field: "#121213",
    groove: "#3a3a3e",
    thumb: "#ffffff",
    border: "#333336",
    borderStrong: "#6e6e73",
    text: "#ececee",
    textSecondary: "#a8a8ad",
    textTertiary: "#909095",
    textDisabled: "#5c5c61",
    accent: "#f5a524",
    accentHover: "#ffb53d",
    onAccent: "#1a1205",
    accentText: "#f7b955",
    accentSubtle: "#3a2a10",
    focus: "#f7b955",
    selection: "#f5a524",
    selectionEdge: "#1a1205",
    playhead: "#ffffff",
    danger: "#ff6b61",
    warning: "#e8d44d",
    success: "#45c4c0",
    clipVideo: "#2d4f8a",
    clipAudio: "#2b6b5c",
    clipText: "#8a5a2b",
    clipEffect: "#6a3f8a",
    onClip: "#ffffff",
    scrim: "rgba(0, 0, 0, 0.5)",
  },
  light: {
    viewer: "#dedee1",
    bg: "#ececee",
    surface: "#f6f6f7",
    surfaceRaised: "#ffffff",
    surfaceHover: "#e6e6e9",
    field: "#ffffff",
    groove: "#d2d2d7",
    thumb: "#ffffff",
    border: "#d8d8dc",
    borderStrong: "#8a8a8f",
    text: "#1a1a1c",
    textSecondary: "#55555a",
    textTertiary: "#68686d",
    textDisabled: "#a4a4a9",
    accent: "#c2560a",
    accentHover: "#a84908",
    onAccent: "#ffffff",
    accentText: "#9a4406",
    accentSubtle: "#fbe8d3",
    focus: "#9a4406",
    selection: "#f5a524",
    selectionEdge: "#1a1205",
    playhead: "#1a1a1c",
    danger: "#c62a21",
    warning: "#6e6000",
    success: "#0b7a75",
    clipVideo: "#2d4f8a",
    clipAudio: "#2b6b5c",
    clipText: "#8a5a2b",
    clipEffect: "#6a3f8a",
    onClip: "#ffffff",
    scrim: "rgba(20, 20, 22, 0.3)",
  },
} as const satisfies Record<ThemeId, Record<string, string>>;

export type ColorToken = keyof (typeof colors)[ThemeId];

export const shadows = {
  dark: {
    shadowPopover: "0 0 0 1px rgba(255, 255, 255, 0.08), 0 8px 24px rgba(0, 0, 0, 0.5)",
    shadowDialog: "0 0 0 1px rgba(255, 255, 255, 0.08), 0 24px 64px rgba(0, 0, 0, 0.6)",
    shadowThumb: "0 0 0 0.5px rgba(0, 0, 0, 0.4), 0 1px 3px rgba(0, 0, 0, 0.5)",
  },
  light: {
    shadowPopover: "0 0 0 1px rgba(0, 0, 0, 0.08), 0 8px 24px rgba(0, 0, 0, 0.14)",
    shadowDialog: "0 0 0 1px rgba(0, 0, 0, 0.08), 0 24px 64px rgba(0, 0, 0, 0.2)",
    shadowThumb: "0 0 0 0.5px rgba(0, 0, 0, 0.2), 0 1px 3px rgba(0, 0, 0, 0.25)",
  },
} as const;

/** Pixel values. */
export const spacing = {
  space2: 2,
  space4: 4,
  space6: 6,
  space8: 8,
  space12: 12,
  space16: 16,
  space24: 24,
  space32: 32,
} as const;

/** Pixel values. */
export const radius = {
  radiusSm: 4,
  radiusMd: 6,
  radiusLg: 10,
  radiusFull: 999,
} as const;

/** Pixel values. */
export const size = {
  controlSm: 22,
  controlMd: 28,
  controlLg: 32,
  toolbar: 36,
  icon: 16,
  trackVideo: 72,
  trackAudio: 48,
  trackAux: 40,
} as const;

export const fonts = {
  sans: "\"Geist Variable\", \"Geist\", system-ui, -apple-system, \"Segoe UI Variable Text\", \"Segoe UI\", sans-serif",
  mono: "\"Geist Mono Variable\", \"Geist Mono\", ui-monospace, \"SF Mono\", \"Cascadia Mono\", Consolas, monospace",
} as const;

export const textStyles = {
  caption: { family: "sans", fontSize: 11, lineHeight: 14, fontWeight: 500 },
  small: { family: "sans", fontSize: 12, lineHeight: 16, fontWeight: 400 },
  body: { family: "sans", fontSize: 13, lineHeight: 18, fontWeight: 400 },
  bodyStrong: { family: "sans", fontSize: 13, lineHeight: 18, fontWeight: 600 },
  title: { family: "sans", fontSize: 15, lineHeight: 20, fontWeight: 600 },
  heading: { family: "sans", fontSize: 20, lineHeight: 26, fontWeight: 600 },
  display: { family: "sans", fontSize: 28, lineHeight: 34, fontWeight: 600 },
  timecode: { family: "mono", fontSize: 12, lineHeight: 16, fontWeight: 500 },
  timecodeLarge: { family: "mono", fontSize: 20, lineHeight: 24, fontWeight: 500 },
} as const;
