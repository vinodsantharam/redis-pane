//! Frame rendering: panes, Viewers, title bar, hint bar.
//!
//! Rendering is a pure function of [`crate::State`]. It draws into a buffer and
//! performs no I/O, which is what lets golden-frame tests exist (ADR-0011).
