# @renderly/core

The project model: time, ids, the schema and loading. No DOM, React or GPU dependency, so it runs
in workers and in plain unit tests (its `tsconfig` has no DOM library at all).

Status: M1. S1.1 (time, ids, schema), S1.2 (commands, store, undo) and S1.3 (timeline edits and
queries) are built.

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

## Commands and the store

Every change to a project is a command: plain data, `{ type, payload }`, checked against the
command's Zod schema. A command's `apply` edits an Immer draft and the store records the patches,
so undo needs no snapshots. Commands are deterministic: ids of new things travel in the payload.

```ts
import { ProjectStore, createId } from "@renderly/core";

const store = new ProjectStore(project);
const result = store.dispatch(
  { type: "marker.add", payload: { sequence, id: createId(), t: 0 } },
  { expectedRevision: store.revision }, // optional: fail with "conflict" if the project moved on
);
// { ok: true, revision, changes: { added: [...], removed: [...], changed: [...] } }
// or { ok: false, error: { code, message, path?, hint } }
```

- `revision` goes up on every change, undo and redo included, and never goes back. It is not
  part of undo.
- `dryRun` returns the change list without committing. `requestId` makes a retry return the first
  result instead of applying twice.
- The whole project is validated after each command; a command that would break it fails with
  `invalid-result` and changes nothing.
- `transaction(label, body)` groups commands into one undo step; returning `false` or throwing
  rolls them back.
- `beginGesture(key, type)` is for sliders and drags: `preview()` many times against the state the
  gesture began from, then `commit()` once for one undo step, or `cancel()`.
- `select(selector, listener)` calls back only when the selected part changed (structural sharing
  makes that a reference check).
- `ViewStore` holds the playhead, zoom, scroll and selection: never in undo or the saved file.

Built-in commands so far: `project.rename`, `marker.add`, `marker.move`, `marker.remove`,
`component.set`.

## Timeline edits

| Command | Does |
|---|---|
| `track.add`, `track.remove`, `track.set` | Add, remove, rename, lock, hide, mute, make magnetic |
| `clip.add` | Place new clips, overwriting or inserting |
| `clip.move` | Move clips in time and across tracks of the same kind, overwriting or inserting |
| `clip.trim` | Move the start or end edge of clips |
| `clip.split` | Cut clips at a time |
| `clip.delete`, `clip.rippleDelete` | Remove clips, leaving a gap or closing it |
| `track.closeGap` | Close the empty space at a time |

The rules after every edit (`checkTimeline(project)` lists any that are broken):

- Clips on a track are sorted and never overlap. Starts and durations are whole frames of the
  sequence (inputs are snapped to the nearest frame), and a clip is at least one frame long.
- A clip never reads outside its source. Trims stop at the source's ends, at neighbouring clips
  and at one frame instead of failing.
- A magnetic track has no gaps. Removing or trimming a clip ripples the clips after it, and a clip
  placed on it goes in at the nearest clip boundary.
- Clips on a locked track cannot be edited. A sequence cannot contain itself, directly or through
  other sequences.
- Clips a command creates without being given an id (the far half of a split or an overwrite) get
  one derived from the original clip's id and the cut time, so a replay gives the same ids.

Queries: `clipsInRange`, `clipAt`, `snapTargets` (with `snapTo`), `sequenceDuration`.

Known limit: splitting a clip drops the far half's keyframes that fall before the cut, so an
animation across the cut changes shape. A key at the cut needs keyframe evaluation, which comes
with the animation work.
