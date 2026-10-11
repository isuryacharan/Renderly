import { defineConfig } from "vitest/config";

// Kept separate from vite.config.ts so the app/harness builds are untouched. Default
// environment is node (the logic modules under test are pure); a file that needs a DOM
// opts in with a `// @vitest-environment jsdom` pragma.
export default defineConfig({
  // commands.test.ts reads ../docs/command-api.md (outside the app root) via ?raw.
  server: { fs: { allow: [".."] } },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
