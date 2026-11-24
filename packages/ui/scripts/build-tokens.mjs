// Generates src/styles/tokens.css and src/tokens.ts from tokens/tokens.json.
// tokens.json is the single source of truth. Never edit the generated files by hand.
//
//   node scripts/build-tokens.mjs          write the files
//   node scripts/build-tokens.mjs --check  exit 1 if the files are out of date

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tokens = JSON.parse(readFileSync(join(root, "tokens/tokens.json"), "utf8"));

const themes = tokens.color.themes.map((t) => t.id);
const [first, ...rest] = themes;
const themed = [...tokens.color.tokens, ...tokens.shadow.tokens];
const plain = [...tokens.spacing.tokens, ...tokens.radius.tokens, ...tokens.size.tokens];

const NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;
const seen = new Set();
for (const t of [...themed, ...plain]) {
  if (!NAME.test(t.name)) throw new Error(`Bad token name: ${t.name}`);
  if (seen.has(t.name)) throw new Error(`Duplicate token name: ${t.name}`);
  if (!t.usage) throw new Error(`Token has no usage note: ${t.name}`);
  seen.add(t.name);
}

const valueFor = (t, theme) =>
  typeof t.value === "string" ? t.value : (t.value[theme] ?? t.value[first]);
const decls = (list, theme, indent = "  ") =>
  list.map((t) => `${indent}--${t.name}: ${valueFor(t, theme)};`).join("\n");
const differs = (theme) => themed.filter((t) => valueFor(t, theme) !== valueFor(t, first));

// ---- tokens.css
let css = `/* Renderly design tokens — generated from tokens/tokens.json. Do not edit. */\n\n`;
css += `:root,\n[data-theme="${first}"] {\n  color-scheme: ${first};\n${decls(themed, first)}\n}\n\n`;
for (const theme of rest) {
  const d = differs(theme);
  css += `[data-theme="${theme}"] {\n  color-scheme: ${theme};\n${decls(d, theme)}\n}\n\n`;
  // Follow the operating system when the app has not set a theme explicitly.
  css += `@media (prefers-color-scheme: ${theme}) {\n  :root:not([data-theme]) {\n    color-scheme: ${theme};\n${decls(d, theme, "    ")}\n  }\n}\n\n`;
}
css += `:root {\n${decls(plain, first)}\n`;
for (const [key, stack] of Object.entries(tokens.type.families))
  css += `  --font-${key}: ${stack};\n`;
css += `}\n\n`;
for (const group of tokens.type.groups) {
  for (const s of group.styles) {
    css += `.text-${s.name} {\n  font-family: var(--font-${s.family ?? group.family});\n  font-size: ${s.fontSize};\n  line-height: ${s.lineHeight};\n  font-weight: ${s.fontWeight};\n`;
    if (s.letterSpacing) css += `  letter-spacing: ${s.letterSpacing};\n`;
    if (group.family === "mono") css += `  font-variant-numeric: tabular-nums;\n`;
    css += `}\n`;
  }
}

// ---- tokens.ts (resolved values, for canvas drawing: timeline, scopes, waveforms)
const camel = (s) => s.replace(/[-.](\w)/g, (_, c) => c.toUpperCase());
const obj = (pairs, indent) =>
  pairs.map(([k, v]) => `${indent}${k}: ${JSON.stringify(v)},`).join("\n");
const px = (v) =>
  typeof v === "string" && /^-?\d+(\.\d+)?px$/.test(v) ? Number(v.slice(0, -2)) : v;

let ts = `// Renderly design tokens — generated from tokens/tokens.json. Do not edit.\n`;
ts += `// Use these resolved values where CSS variables cannot reach: canvas drawing for the\n// timeline, scopes and waveforms. Everywhere else, use the CSS variables in tokens.css.\n\n`;
ts += `export type ThemeId = ${themes.map((t) => JSON.stringify(t)).join(" | ")};\n\n`;
ts += `export const themeIds: readonly ThemeId[] = ${JSON.stringify(themes)};\n\n`;
ts += `export const colors = {\n`;
for (const theme of themes)
  ts += `  ${theme}: {\n${obj(
    tokens.color.tokens.map((t) => [camel(t.name), valueFor(t, theme)]),
    "    ",
  )}\n  },\n`;
ts += `} as const satisfies Record<ThemeId, Record<string, string>>;\n\n`;
ts += `export type ColorToken = keyof (typeof colors)[ThemeId];\n\n`;
ts += `export const shadows = {\n`;
for (const theme of themes)
  ts += `  ${theme}: {\n${obj(
    tokens.shadow.tokens.map((t) => [camel(t.name), valueFor(t, theme)]),
    "    ",
  )}\n  },\n`;
ts += `} as const;\n\n`;
ts += `/** Pixel values. */\nexport const spacing = {\n${obj(
  tokens.spacing.tokens.map((t) => [camel(t.name), px(t.value)]),
  "  ",
)}\n} as const;\n\n`;
ts += `/** Pixel values. */\nexport const radius = {\n${obj(
  tokens.radius.tokens.map((t) => [camel(t.name), px(t.value)]),
  "  ",
)}\n} as const;\n\n`;
ts += `/** Pixel values. */\nexport const size = {\n${obj(
  tokens.size.tokens.map((t) => [camel(t.name), px(t.value)]),
  "  ",
)}\n} as const;\n\n`;
ts += `export const fonts = {\n${obj(Object.entries(tokens.type.families), "  ")}\n} as const;\n\n`;
ts += `export const textStyles = {\n`;
for (const group of tokens.type.groups) {
  for (const s of group.styles) {
    ts += `  ${camel(s.name)}: { family: ${JSON.stringify(s.family ?? group.family)}, fontSize: ${px(s.fontSize)}, lineHeight: ${px(s.lineHeight)}, fontWeight: ${s.fontWeight} },\n`;
  }
}
ts += `} as const;\n`;

// ---- write or check
const targets = [
  [join(root, "src/styles/tokens.css"), css],
  [join(root, "src/tokens.ts"), ts],
];
const check = process.argv.includes("--check");
let stale = false;
for (const [file, content] of targets) {
  const current = existsSync(file) ? readFileSync(file, "utf8").replace(/\r\n/g, "\n") : null;
  if (current === content) continue;
  if (check) {
    console.error(`Out of date: ${file}`);
    stale = true;
  } else {
    writeFileSync(file, content);
    console.log(`Wrote ${file}`);
  }
}
if (check && stale) {
  console.error("Run: node scripts/build-tokens.mjs");
  process.exit(1);
}
if (check && !stale) console.log("Tokens are up to date.");
