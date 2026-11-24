# @renderly/ui

The Renderly design system as code: tokens, base styles and component styles.

The rules for using it are in [docs/design-system/README.md](../../docs/design-system/README.md).
To see everything rendered, open
[docs/design-system/preview/index.html](../../docs/design-system/preview/index.html) in a browser.

## Layout

```
tokens/tokens.json          source of truth: every token with a usage note
scripts/build-tokens.mjs    generates the two files marked "generated"
scripts/build-preview.mjs   generates the preview page from its template
src/tokens.ts               generated: resolved values for canvas drawing
src/styles/tokens.css       generated: CSS variables and .text-* classes
src/styles/fonts.css        the interface typefaces (Geist, Geist Mono)
src/styles/base.css         page defaults, focus ring, selection, motion
src/styles/components/      one stylesheet per component, .r-* classes
src/styles/index.css        imports everything except fonts
```

## Using it

```ts
import "@renderly/ui/fonts.css";   // once, at the app entry
import "@renderly/ui/styles.css"; // tokens, base and components
```

```html
<button class="r-button r-button--primary">Export</button>

<div class="r-field r-field--number">
  <input type="text" inputmode="decimal" value="29.97">
  <span class="r-field__unit">fps</span>
</div>

<input class="r-slider" type="range" min="0" max="100" value="72" style="--value: 72%">
```

For canvas drawing, where CSS variables cannot reach:

```ts
import { colors, size } from "@renderly/ui/tokens";

ctx.fillStyle = colors[theme].clipVideo;
const trackHeight = size.trackVideo; // 72
```

## Themes

Dark is the default. Set `data-theme="light"` or `data-theme="dark"` on the root element to force
a theme. With no attribute, the page follows the operating system.

## Changing a token

1. Edit `tokens/tokens.json`. Every token needs a `usage` note saying where it is used.
2. Run `node scripts/build-tokens.mjs` (or `pnpm build:tokens`).
3. Run `node scripts/build-preview.mjs` and check the preview page in both themes.
4. Commit `tokens.json` and the generated files together.

`node scripts/build-tokens.mjs --check` fails if the generated files are out of date. Run it in CI.

## Rules

- Never hard-code a colour, size, radius or shadow in a component. Use the variable.
- Text on an accent fill uses `--on-accent`. It is not always white.
- Marks drawn on clips or on the picture use `--selection` with a `--selection-edge` line.
- A slider's fill comes from its `--value` custom property. Keep it in sync with the value.
- Styles here are plain CSS with no build step of their own. React components that wrap these
  classes are added in slice S1.5.
