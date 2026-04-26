//! Crawl-related types shared across command handlers.
//!
//! Purpose: Define crawl source types and failure tracking.
//! Edit here when: Adding new crawl source types or changing failure tracking fields.
//! Do not edit here for: Pipeline logic (see pipeline.rs), CLI handlers (see commands/crawl.rs).

/// Failure tracking for crawl pipeline.
///
/// With LanceDB storage, structural errors (disk full, dataset corruption) cause
/// immediate abort. Only embedding failures are tracked per-chunk, as these can
/// fail for tokenizer edge cases or model issues on specific content.
#[derive(Debug, Default)]
pub struct CrawlFailures {
    /// Chunks that failed to embed (per-chunk tokenizer/model issues)
    pub embedding_failures: Vec<String>,
}

impl CrawlFailures {
    pub fn total(&self) -> usize {
        self.embedding_failures.len()
    }

    pub fn has_failures(&self) -> bool {
        self.total() > 0
    }
}
