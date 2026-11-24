// The JSON Schema in docs/schema/ is generated from the Zod schema. This test fails when the
// committed file is out of date; `pnpm --filter @renderly/core build:schema` rewrites it.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { z } from "zod";
import { CURRENT_VERSION, projectSchema } from "./index";

const file = join(
  import.meta.dirname,
  "..",
  "..",
  "..",
  "docs",
  "schema",
  `project.v${CURRENT_VERSION}.schema.json`,
);

function generate(): string {
  const schema = z.toJSONSchema(projectSchema(), { io: "input", unrepresentable: "any" });
  const document = {
    $id: `https://renderly.dev/schema/project.v${CURRENT_VERSION}.schema.json`,
    title: `Renderly project, version ${CURRENT_VERSION}`,
    description:
      "Generated from packages/core/src/schema.ts; do not edit. Times are integer flicks (1/705,600,000 s). " +
      "Some rules (unique ids, references, supported frame rates) are checked by the app, not by this schema.",
    ...schema,
  };
  return `${JSON.stringify(document, null, 2)}\n`;
}

describe("generated JSON Schema", () => {
  it("is up to date in docs/schema/", () => {
    const fresh = generate();
    if (process.env["UPDATE_SCHEMA"]) writeFileSync(file, fresh);
    let committed = "";
    try {
      committed = readFileSync(file, "utf8");
    } catch {
      // missing: fails below
    }
    expect(committed, "Run `pnpm --filter @renderly/core build:schema`").toBe(fresh);
  });
});
