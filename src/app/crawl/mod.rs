//! Crawl pipeline implementation.
//!
//! Edit here when: Modifying the embed/upload pipeline or crawl types.
//! Do not edit here for: Crawl command handlers (see `../commands/crawl.rs`).

pub mod pipeline;
pub mod types;

pub use pipeline::run_embed_upload_pipeline;
pub use types::CrawlFailures;
