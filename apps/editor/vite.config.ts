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
});
