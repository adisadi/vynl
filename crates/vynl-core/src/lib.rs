//! Core pure-Rust domain layer for `vynl`.
//!
//! This crate intentionally performs **no I/O** so that state transitions,
//! responsive layout breakpoints, configuration defaults, and WCAG-AA theme
//! contrast math can be tested deterministically across every target platform.

pub mod config;
pub mod model;
pub mod state;
pub mod theme;
