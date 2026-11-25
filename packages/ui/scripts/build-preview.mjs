// Generates docs/design-system/preview/index.html from index.template.html:
// fills in the colour swatches and type samples from tokens/tokens.json, the icons from
// icons/icons.json, and inlines the timeline reference.
//
//   node scripts/build-preview.mjs

import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const previewDir = join(root, "../../docs/design-system/preview");
const tokens = JSON.parse(readFileSync(join(root, "tokens/tokens.json"), "utf8"));
const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;");

const swatches = tokens.color.tokens
  .map(
    (t) =>
      `<div class="g-swatch"><i style="background:var(--${t.name})"></i><b>${t.name}</b><span>${esc(t.usage)}</span></div>`,
  )
  .join("\n      ");

const type = tokens.type.groups
  .flatMap((g) =>
    g.styles.map(
      (s) =>
        `<div><code>${s.name} · ${s.fontSize}/${s.lineHeight} · ${s.fontWeight}</code><span class="text-${s.name}">${esc(s.sample ?? s.name)}</span></div>`,
    ),
  )
  .join("\n      ");

const tablerDir = dirname(
  createRequire(join(root, "package.json")).resolve("@tabler/icons/package.json"),
);
const tabler = JSON.parse(readFileSync(join(tablerDir, "tabler-nodes-outline.json"), "utf8"));
const iconList = JSON.parse(readFileSync(join(root, "icons/icons.json"), "utf8")).icons;
/** An icon as inline SVG, the way the Icon component draws it. */
const svg = (icon) => {
  const shapes = tabler[icon.tabler]
    .map(
      ([tag, attrs]) =>
        `<${tag} ${Object.entries(attrs)
          .filter(([k]) => k !== "key")
          .map(([k, v]) => `${k}="${esc(v)}"`)
          .join(" ")}/>`,
    )
    .join("");
  return `<svg class="r-icon" viewBox="0 0 24 24" aria-hidden="true">${shapes}</svg>`;
};
const icons = iconList
  .map(
    (icon) =>
      `<div class="g-icon" title="${esc(icon.usage)}">${svg(icon)}<code>${icon.name}</code></div>`,
  )
  .join("\n      ");

// The timeline reference is kept as its own fragment so it can be edited alone.
let timeline = readFileSync(join(previewDir, "timeline.fragment.html"), "utf8");

let html = readFileSync(join(previewDir, "index.template.html"), "utf8");
for (const [slot, content] of [
  ["<!--SWATCHES-->", swatches],
  ["<!--TYPE-->", type],
  ["<!--ICONS-->", icons],
  ["<!--TIMELINE-->", timeline],
]) {
  if (!html.includes(slot)) throw new Error(`Template is missing ${slot}`);
  html = html.replace(slot, () => content);
}
// <!--icon:warning--> anywhere in the template becomes that icon.
html = html.replace(/<!--icon:([a-z-]+)-->/g, (_, name) => {
  const icon = iconList.find((i) => i.name === name);
  if (!icon) throw new Error(`The template uses an unknown icon "${name}"`);
  return svg(icon);
});
writeFileSync(join(previewDir, "index.html"), html);
console.log("Wrote docs/design-system/preview/index.html");
