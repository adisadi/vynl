//! Core pure-Rust domain layer for `vynl`.
//!
//! This crate intentionally performs **no I/O** so that state transitions,
//! responsive layout breakpoints, TOML config/theme parsing, fuzzy matching,
//! keybinding resolution, and WCAG-AA theme contrast math can be tested
//! deterministically across every target platform.

pub mod config;
pub mod fuzzy;
pub mod keymap;
pub mod model;
pub mod state;
pub mod theme;
