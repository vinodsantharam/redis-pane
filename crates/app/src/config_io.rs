//! Reading `~/.config/redis-pane/config.json` (ADR-0002, ADR-0003).
//!
//! The app only ever reads this file. It is refused when group- or
//! world-readable, because it may carry a literal password. Parsing and
//! validation live in `redis_pane_core::config`; this module only does I/O.
