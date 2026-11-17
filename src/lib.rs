//! REST Harness core: pure-Rust domain model and git-friendly YAML
//! filesystem persistence (see `GLOSSARY.md` and `docs/adr/`).
//!
//! Downstream tasks (interpolation/HTTP engine, the `gpui` GUI) build on
//! this crate's public API:
//! - [`domain`]: Workspace / Collection / Folder / Request / Environment /
//!   SecretStore / HistoryEntry models.
//! - [`storage`]: the [`storage::WorkspaceStorage`] port and its
//!   [`storage::FsWorkspaceStorage`] implementation, plus filename
//!   sanitization and `.gitignore` management.
//! - [`config`]: app config persistence and Workspace resolution/seeding.
//! - [`history`]: gitignored History Entry store with Secret redaction.
//! - [`app`]: UI-agnostic GUI core (`AppState` + `Message` + `Effect`
//!   reducer, tree projection, CRUD/filter logic). This module — like the
//!   rest of the crate — must stay free of GUI-toolkit types; the `gpui`
//!   shell lives in the binary's `gui` module (`src/gui/`).

pub mod app;
pub mod config;
pub mod domain;
pub mod error;
pub mod history;
pub mod http;
pub mod interpolation;
pub mod storage;
pub mod url_query;

pub use error::{Error, Result};

/// Crate version from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
