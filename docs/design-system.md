# Renderly design system

Status: current (improvement-plan Workstream D). Source of truth for UI theming and
component conventions in `renderly-app`. The token definitions live in
[`renderly-app/src/styles/tokens.css`](../renderly-app/src/styles/tokens.css); this doc
explains the architecture and the rules that keep it consistent.

## Token architecture (two layers)

**Layer 1 — primitives.** Theme-agnostic HSL scales defined once on `:root`:

- `--neutral-{50..950}` — slightly cool gray (hue 220), the base of both themes.
- `--accent-{50..950}` — calm professional blue (hue 215). The former ember-orange brand
  accent is retired.
- `--danger-500/600`, `--success-500`, `--warning-500` — semantic state scales.
- Non-color primitives shared by both themes: `--radius-s/m/l`, `--space-1..6`,
  `--font-ui`, `--fs-11..15`, layout constants (`--track-label-w`, `--ruler-h`,
  `--timeline-h`, `--titlebar-h`).

**Layer 2 — semantic tokens.** Defined per theme under `:root[data-theme="dark"]`
(also the bare `:root` default) and `:root[data-theme="light"]`. Components and canvas
code consume **only** these, never primitives or raw colors:

| Group | Tokens |
|---|---|
| Surfaces | `--bg-0..3`, `--border`, `--border-strong`, `--shadow` |
| Text | `--text-1..3`, `--muted` |
| Accent | `--accent`, `--accent-hover`, `--accent-active`, `--accent-soft`, `--on-accent` |
| State | `--danger`, `--danger-soft`, `--warning`, `--success`, `--destructive` |
| Timeline | `--timeline-bg`, `--ruler-bg/-text/-line-strong/-line-weak`, `--track-header-bg`, `--track-lane-bg`, `--playhead`, `--snap-guide`, `--locked-hatch`, `--clip-shadow` |
| Clips | `--clip-video-bg`, `--clip-audio-bg`, `--clip-audio-wave`, `--clip-caption-bg`, `--clip-adjustment-bg/-edge` (amber), `--clip-sequence-bg/-edge` (magenta; nested sequences and their media-bin thumbnails), `--clip-border-selected` |

## Themes

- Dark is the default; light is selectable (persisted preference). Theme selection is
  `"light" | "dark" | "system"` via `src/lib/theme.ts` (`setTheme`, `getThemePreference`,
  `resolveTheme`); it writes `data-theme` on `<html>` and sets `color-scheme`, and the
  `"system"` preference follows `prefers-color-scheme` live.
- **The timeline stays darker than the chrome in both themes** (pro-NLE convention —
  waveforms, thumbnails, and clip text keep their contrast). Light mode lightens chrome to
  near-white but only slightly lightens timeline surfaces.
- The theme-cycle action lives in the actions registry (`view.cycle-theme`) and the TopBar.

## Canvas bridge

The canvas timeline cannot read CSS classes, so `src/timeline/theme.ts` resolves the
semantic tokens above into a cached JS theme object. Rules:

- **No color literals in canvas code** — every color the renderer uses must come from a
  token via `getTimelineTheme()`. The same rule now applies to all CSS: `globals.css`
  contains no raw hex/rgba values, only `var(--…)` references.
- On theme change, call `resetTimelineThemeCache()` and force a full timeline re-render
  (the store bumps `themeEpoch` for components that render theme-dependent output outside
  React's data flow).

## Component inventory (`src/components/ui/`)

Current primitives: `IconButton`, `MenuSelect`, `Tooltip`, `WindowControls`, plus `Toasts`
at `src/components/`. Grow this inventory opportunistically (Slider, Tabs, Dialog, Toggle,
Field/Label) as feature work touches the corresponding areas — do not big-bang refactor.
New components must:

1. Use semantic tokens only (no primitives, no literals).
2. Render correctly under both `data-theme` values before merging.
3. Take labels/shortcuts from the actions registry (`src/lib/actions.ts`) when they invoke
   an editor operation, rather than hardcoding strings.

## CSS conventions

- `globals.css` is being split by component area as areas get touched; new styles go in
  per-area files, not appended to `globals.css`.
- No Tailwind or CSS-in-JS — plain CSS + custom properties.
- Spacing uses `--space-*`; radii use `--radius-*`; font sizes use `--fs-*`.
