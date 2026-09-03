//! The terminal shell: raw mode, the event loop, and resize handling (PLAN M0.5).
//!
//! The render loop never does I/O. Keystrokes become [`redis_pane_core::Msg`]s
//! and are answered within one frame regardless of what the network is doing.
