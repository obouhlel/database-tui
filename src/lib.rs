//! pgtui — a keyboard-driven PostgreSQL client for the terminal.
//!
//! Layers:
//! - [`config`]: `config.toml` parsing and password resolution;
//! - [`db`]: the [`db::Backend`] trait, the PostgreSQL implementation and the
//!   worker task that owns connections;
//! - [`app`]: state + reducer (no I/O);
//! - [`ui`]: rendering (no I/O, no state changes);
//! - [`keymap`]: key sequences to actions, user-overridable;
//! - [`run`]: the event loop wiring everything together.

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod db;
pub mod keymap;
pub mod logging;
pub mod run;
pub mod sql;
pub mod terminal;
pub mod ui;
