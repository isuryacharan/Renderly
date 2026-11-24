import { defineConfig } from "vite";

// Cross-origin isolation is required for SharedArrayBuffer (the audio ring buffer in S0.4)
// and for precise performance timers. Both the dev server and the preview server send it.
const crossOriginIsolation = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
};

export default defineConfig({
  server: { headers: crossOriginIsolation },
  preview: { headers: crossOriginIsolation },
  worker: { format: "es" },
});
