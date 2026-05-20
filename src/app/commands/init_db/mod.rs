//! init-db command folder.
//!
//! Purpose: Facade for the init-db command; re-exports the public entry point.
//! Edit here when: Adding or renaming init-db submodules, or changing the public surface re-exported from this folder.
//! Do not edit here for: init-db behavior (see `run.rs`), tests (see `tests.rs`).

mod run;
#[cfg(test)]
mod tests;

pub use run::run_init_db;
