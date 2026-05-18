//! Crawl pipeline orchestration shared between command handlers.
//!
//! Purpose: Export crawl submodules and public surface for command handlers.
//! Edit here when: Adding or removing crawl submodules, or changing the public surface
//! re-exported from this directory.
//! Do not edit here for: Phase orchestration (see `phases.rs`), embed/upload pipeline
//! (see `pipeline.rs`), crawl types (see `types.rs`), warning handling (see `warning.rs`),
//! or crawl command handlers (see `../commands/crawl.rs`).

pub mod phases;
pub mod pipeline;
pub(crate) mod preamble;
pub mod types;
pub mod warning;

pub use pipeline::{run_embed_upload_pipeline, run_upsert_without_vectors};
pub use types::{CrawlFailures, PhaseResults};
