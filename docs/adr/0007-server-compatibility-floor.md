# ADR-0007 — Server compatibility floor

**Status:** Accepted · **Date:** 2026-08-26

## Context

[ADR-0006](0006-liveness-without-a-refresh-button.md) builds the product's headline behaviour on
RESP3 `CLIENT TRACKING`, which arrived in Redis 6.0 (2020). That raises a question the documents
had left implicit: what is the oldest server `redis-pane` will talk to at all, and does it speak
RESP2 as well?

The two are separable. A RESP2 fallback would reach Redis 4 and 5 — plausibly the legacy
infrastructure where people are most stuck with `redis-cli`. But RESP2 and RESP3 disagree about
reply *shape* for several commands a type-aware Viewer depends on, so supporting both means
carrying two paths through the layer that matters most. `fred` normalises much of this, which
makes the tax smaller than it first appears, but not zero.

A second finding turned out to matter more than the version question. Support for
`CLIENT TRACKING` does not follow from the version number. AWS ElastiCache *Serverless* answers
`CLIENT TRACKING` with `unknown subcommand 'tracking'` on otherwise-current Redis, while
self-managed ElastiCache 7.1 handles it normally. Managed platforms restrict `CLIENT`
subcommands on their own schedule.

## Decision

**RESP3 only. Redis 6.0 or later, and API-compatible forks (Valkey).**

A server below the floor is refused at launch with a diagnostic naming the version found, in the
same shape as any other startup failure ([ADR-0009](0009-connection-lifecycle.md)).

**Liveness is gated by capability, not by version.** The app attempts `CLIENT TRACKING` and
degrades to `○ manual` on error. Version is never used as a proxy for the feature.

## Alternatives considered

**RESP2 baseline with opportunistic RESP3.** Rejected. It reaches Redis 4 and 5, but the cost
lands in the Viewer layer — the part of the product with the most surface and the most detail —
and it is paid on every type, forever, to serve servers that have been end-of-life for years.

**RESP3 with a Redis 7.0 floor.** Rejected. 6.2 is widely deployed and long-lived; the extra
narrowing buys little and cuts off real users.

**Detecting liveness support from `INFO server`.** Rejected on evidence. The ElastiCache
Serverless case proves version does not imply capability, and a version check would leave the
app asserting `● live` against a server that never agreed to tell it anything — the precise
failure ADR-0006 exists to prevent.

## Consequences

- One protocol, one reply shape, one code path per type Viewer.
- The `○ manual` path is **production infrastructure, not a legacy path**. It must be built to
  the same standard as the live path: Read age visible, `r` bound, and the reason stated.
- Capability probing happens once per Connection, at connect and on every reconnect.
- The test matrix is Redis 6.2, Redis 7.x, and Valkey, plus one deployment that refuses
  `CLIENT TRACKING` to exercise the fallback.
- Anyone on Redis 5 or older is told plainly, at launch, with the version and the floor. Being
  refused with a reason is better than being admitted into a hollowed-out product.
