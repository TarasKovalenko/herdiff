//! herdiff: a live diff viewer for the git repos herdr panes are working in.
//!
//! The binary in `main.rs` wires these modules into a terminal app. They're a library so
//! `examples/screenshots.rs` can drive the real rendering code.

pub mod app;
pub mod diff;
pub mod git;
pub mod herdr;
pub mod highlight;
pub mod model;
pub mod scope;
pub mod ui;
pub mod worker;
