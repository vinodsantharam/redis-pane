//! The Redis shell: connection, capability probe, and tracking (PLAN M0.8–M0.10).
//!
//! The server floor is RESP3 and Redis 6.0 (ADR-0007). Liveness is gated by
//! *capability*, never by version — managed platforms refuse `CLIENT TRACKING`
//! independently of the version they report.
//!
//! Two re-arm invariants hold here, both verified against Redis 8.4.0 (ADR-0006):
//!
//! 1. Every Refetch re-arms, because tracking is consumed by the invalidation it
//!    produces.
//! 2. Every reconnect re-arms before anything claims to be live.
//!
//! Breaking either leaves the Viewer dark while the header still reads `live`,
//! which is the defect this project exists to fix.
