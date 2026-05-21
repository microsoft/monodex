//! Crawl pipeline orchestration shared between command handlers.
//!
//! Purpose: Export crawl submodules and public surface for command handlers.
//! Edit here when: Adding or removing crawl submodules, or changing the public surface
//! re-exported from this folder.
//! Do not edit here for: Phase orchestration (see `phases.rs`), embed/upload pipeline
//! (see `pipeline.rs`), crawl types (see `types.rs`), warning handling (see `warning.rs`),
//! shared preamble setup (see `preamble.rs`), summary/warning rendering (see `summary.rs`),
//! progress/time display vocabulary (see `progress_format.rs`),
//! or crawl command handlers (see `../commands/crawl.rs`).

mod phases;
mod pipeline;
mod preamble;
mod progress_format;
mod summary;
mod types;
mod warning;

pub use phases::{
    ChunkingOutput, add_label_to_existing_files, build_package_index, chunk_new_files,
    classify_files, enumerate_files, filter_files, open_storage, run_fts_phase, run_label_cleanup,
    update_final_metadata, write_in_progress_metadata,
};
pub use pipeline::{run_embed_upload_pipeline, run_upsert_without_vectors};
pub use preamble::print_narrowing_announcement;
pub(crate) use preamble::{CrawlInput, CrawlPreamble, prepare_crawl_preamble};
pub use summary::{print_summary, print_warning_summary};
pub use types::{CrawlFailures, CrawlSourceMetadata, PhaseResults};
pub use warning::create_warning_sink;
