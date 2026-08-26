# ADR-0010 — Keyspace memory model

**Status:** Accepted · **Date:** 2026-08-26

## Context

Two requirements contradicted each other. R2.5 offers sorting the key list by name, TTL or size;
R2.6 requires handling 1M+ keys in bounded memory, with §7 setting the budget at 250MB RSS. A
sort needs the whole set. A bounded window is, by definition, not the whole set.

The intuitive resolution — hold a sliding window and re-`SCAN` when the user scrolls — is worse
than it sounds. `SCAN` cursors are forward-only with no random access, the iteration order is
unspecified, and a full iteration may return the same key more than once. A window cannot seek
to row 500,000; scrolling backwards means replaying cursors or retaining a cursor history, and
the visible set can shift underneath the reader. It is more code, more failure modes, and worse
behaviour than the thing it was meant to avoid.

Holding everything is cheap in Rust if the layout is chosen deliberately. Key names in a single
byte arena addressed by `(offset, len)`, metadata in parallel arrays rather than a `Vec` of
structs: roughly 45MB of names plus ~15MB of indices at 1M keys, well inside the budget. Sorting
permutes an index vector and never touches the data.

## Decision

**All scanned keys are retained, in a columnar arena, up to a hard cap.**

- Key names live in one contiguous byte arena; each key is an `(offset, len)` pair.
- Metadata is stored as parallel arrays — type, TTL, size — not as per-key structs.
- Sorting and filtering operate over index vectors spanning the entire **Loaded set**.
- On reaching the cap, scanning stops and the status bar says so and suggests narrowing the
  filter. Memory is bounded by a documented number, not by hope.

**Sorting by a lazily-fetched column orders what has arrived.** `SIZE` is fetched on demand
(R2.4), so sorting 41,203 keys of which 4,120 sizes are known orders those, parks the remainder
at the end, and states the count in the header. Silently issuing a million `MEMORY USAGE`
commands because someone pressed a sort key is the behaviour this project exists to replace.

## Alternatives considered

**Sliding window with re-`SCAN` on scroll.** Rejected, and worth recording explicitly because it
is the answer that sounds responsible. `SCAN`'s semantics make it strictly harder and strictly
worse; see Context.

**Hold everything with no cap.** Rejected. It is what most tools do and it is the least code, but
a keyspace an order of magnitude past the target takes the process out with the OOM killer — on
a bastion host, during an incident, which is the worst possible moment to discover an unstated
limit.

**Blocking metadata fetch before sorting.** Rejected for the reason above. Offered instead as an
explicit, cancellable action for a user who genuinely wants it on a set small enough to warrant
it.

## Consequences

- R2.5 and R2.6 are reconciled: sort and filter span the Loaded set, and the Loaded set is
  bounded by the cap.
- "Loaded set" becomes vocabulary ([CONTEXT.md](../../CONTEXT.md)), because the scope of every
  sort, filter and bulk operation is now a thing the UI must be able to name.
- The columnar layout is difficult to retrofit — it shapes every access to the key list — so it
  belongs in the first commit that stores a key, not in the performance milestone.
- The cap is a documented number and a visible state, not an implementation detail.
- Tree mode (R2.3) builds a prefix index over the same arena rather than a second copy of the
  key names.
