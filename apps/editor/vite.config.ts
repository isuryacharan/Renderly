import { defineConfig } from "vite";

// Cross-origin isolation: SharedArrayBuffer (the audio clock) and precise timers need it.
const crossOriginIsolation = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
};

export default defineConfig({
  server: { port: 5180, headers: crossOriginIsolation },
  preview: { port: 5180, headers: crossOriginIsolation },
  worker: { format: "es" },
  // Scan the dev-only media lab too, so its dependencies are bundled at start-up instead of on its
  // first visit (which reloads the page in the middle of a test).
  optimizeDeps: { entries: ["index.html", "media-lab.html"] },
});
