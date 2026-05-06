//! Crawl-related types shared across command handlers.
//!
//! Purpose: Define crawl source types and failure tracking.
//! Edit here when: Adding new crawl source types or changing failure tracking fields.
//! Do not edit here for: Pipeline logic (see pipeline.rs), CLI handlers (see commands/crawl.rs), phase output types (see phases.rs).

use crate::engine::retrieval::RetrievalMethod;
use std::collections::BTreeSet;

/// Results from each phase of a crawl, used for final metadata update.
///
/// Each field tracks whether a phase succeeded. `None` means the phase
/// wasn't run (method not in selection). `Some(false)` means the phase
/// ran but failed.
pub struct PhaseResults {
    /// Whether the vector/embed phase succeeded. None = vector not in selection.
    pub vector_succeeded: Option<bool>,
    /// Whether the FTS indexing phase succeeded. None = fts not in selection.
    pub fts_succeeded: Option<bool>,
    /// Whether label reassignment succeeded. Always required for completion.
    pub label_reassignment_succeeded: bool,
}

impl PhaseResults {
    /// Create PhaseResults for a given selection, with pessimistic defaults.
    pub fn new(selection: &BTreeSet<RetrievalMethod>) -> Self {
        Self {
            vector_succeeded: if selection.contains(&RetrievalMethod::Vector) {
                Some(false)
            } else {
                None
            },
            fts_succeeded: if selection.contains(&RetrievalMethod::Fts) {
                Some(false)
            } else {
                None
            },
            label_reassignment_succeeded: false,
        }
    }
}

/// Metadata about the crawl source that travels alongside a `BlobSource`.
///
/// This struct carries source identity (source_kind, source_value) as pure data.
/// It does NOT include behavior - that's what the `BlobSource` trait is for.
/// The separation ensures the merged crawl flow doesn't branch on source identity.
///
/// Use `SOURCE_KIND_GIT_COMMIT` and `SOURCE_KIND_WORKING_DIRECTORY` from
/// `crate::engine::storage` for the `source_kind` field.
///
/// The `source_value` field carries either a real commit OID (commit mode)
/// or a working-directory sentinel string (working-dir mode).
pub struct CrawlSourceMetadata {
    /// The source kind string: `"git-commit"` or `"working-directory"`.
    pub source_kind: &'static str,
    /// The resolved commit SHA for commit mode, or a working-directory sentinel
    /// for working-directory mode. The sentinel is per-crawl-unique so any two
    /// working-dir crawls compare unequal.
    pub source_value: String,
}

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
