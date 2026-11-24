import { resolve } from "node:path";
import { defineConfig } from "vite";
import { testMedia } from "./test-media-plugin";

// Cross-origin isolation is required for SharedArrayBuffer (the audio ring buffer in S0.4)
// and for precise performance timers. Both the dev server and the preview server send it.
const crossOriginIsolation = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
};

export default defineConfig({
  plugins: [testMedia(resolve(import.meta.dirname, "../../test-media"))],
  server: { headers: crossOriginIsolation },
  preview: { headers: crossOriginIsolation },
  worker: { format: "es" },
  build: {
    rollupOptions: {
      input: {
        index: resolve(import.meta.dirname, "index.html"),
        decode: resolve(import.meta.dirname, "decode.html"),
        preview: resolve(import.meta.dirname, "preview.html"),
      },
    },
  },
});
