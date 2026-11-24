# Decision records

One file per decision that settles a design question: what was decided, why, and what it costs.
Records are numbered in order (`0001-engine.md`, `0002-…`) and never renumbered.

To add one, copy [template.md](template.md) to the next number and fill it in. Add a one-line
summary to the Decisions table in [MEMORY.md](../../MEMORY.md) in the same change.

A record that is replaced is not deleted. Set its status to "Superseded by NNNN" and link the new
record.

| # | Decision | Status |
|---|---|---|
| [0001](0001-engine.md) | The WebCodecs engine: go | Accepted |
