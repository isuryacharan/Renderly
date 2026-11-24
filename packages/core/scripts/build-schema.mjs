// Regenerates docs/schema/project.v<N>.schema.json by running its test in write mode.
import { spawnSync } from "node:child_process";
import { join } from "node:path";

const root = join(import.meta.dirname, "..", "..", "..");
const result = spawnSync("pnpm vitest run packages/core/src/json-schema.test.ts", {
  cwd: root,
  shell: true,
  stdio: "inherit",
  env: { ...process.env, UPDATE_SCHEMA: "1" },
});
process.exit(result.status ?? 1);
