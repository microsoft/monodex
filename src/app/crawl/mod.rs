//! Purpose: Crawl pipeline orchestration shared between command handlers — phases, types, embed/upload pipeline.
//! Edit here when: Modifying the embed/upload pipeline, crawl-phase wiring, or crawl types.
//! Do not edit here for: Crawl command handlers (see `../commands/crawl.rs`), engine-level git/storage/chunking code (see `../../engine/`).

pub mod phases;
pub mod pipeline;
pub mod types;
pub mod warning;

pub use pipeline::run_embed_upload_pipeline;
pub use types::CrawlFailures;
