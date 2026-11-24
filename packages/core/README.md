# @renderly/core

The project model: time, ids, the schema and loading. No DOM, React or GPU dependency, so it runs
in workers and in plain unit tests (its `tsconfig` has no DOM library at all).

Status: M1, slice S1.1. Commands, undo and queries come in S1.2 and S1.3.

## Time

Every time is an integer number of flicks (1/705,600,000 s). Frame rates are exact fractions,
`[30, 1]` or `[30000, 1001]`. Supported: 24, 25, 30, 50, 60, 90, 100 and 120 fps and each NTSC
variant (÷1.001); 32, 44.1, 48, 88.2 and 96 kHz audio. At each of these a frame or sample is a
whole number of flicks, so conversions are exact.

```ts
import { framesToFlicks, flicksToFrame, snapToFrame, formatTimecode } from "@renderly/core";

const ntsc = [30000, 1001] as const;
framesToFlicks(1800, ntsc);                          // 42,383,736,000 flicks, exactly
formatTimecode(framesToFlicks(1800, ntsc), ntsc);    // "00:01:00;02" (drop-frame)
```

Times past about ±147 days do not fit in an exact integer, and the conversions refuse them
instead of rounding.

## Projects

```ts
import { parseProject, serialiseProject } from "@renderly/core";

const result = parseProject(text);
if (!result.ok) {
  // result.reason: "invalid" with problems [{ path: "sequences[0].fps", message: "…" }],
  // or "newer" with the version: a project from a newer app is refused, never half-loaded.
}
```

- `schema.ts` defines version 1 once, with Zod. `docs/schema/project.v1.schema.json` is generated
  from it; a test fails when it is out of date (`pnpm --filter @renderly/core build:schema`).
- `migrate()` checks the format, refuses newer versions, runs one pure upgrade function per older
  version, then validates. It also checks what a schema cannot: unique ids, references that
  resolve, a compound clip not containing its own sequence.
- `serialiseProject()` writes fields in schema order with two-space indentation; a canonical
  document round-trips byte for byte (`fixtures/holiday-edit.project.json`).

## Components

A clip's features are components. `registerComponent(type, fields)` adds a type; the clip type
never changes. Every component may carry keyframes under `keys`. Built in so far: `transform`.
