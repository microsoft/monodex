//! Purpose: Shared crawl-preamble preparation for the `crawl` command's two entry points (commit and working-directory), plus startup-time retrieval-selection messaging.
//! Edit here when: changing what setup steps are performed before the post-chunking phases run, adding a new crawl source kind, or modifying the narrowing-announcement / retrieval-selection display.
//! Do not edit here for: blob-source construction at the command boundary (see commands/crawl.rs), the post-chunking phase pipeline (see crawl/phases.rs), lock primitives (see engine/storage/locks.rs), or completion/warning summary rendering (see summary.rs).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, anyhow};

use crate::app::config::Config;
use crate::app::crawl::types::CrawlSourceMetadata;
use crate::app::util::stderr_lock_progress;
use crate::app::{resolve_database_path, validate_config_path};
use crate::engine::crawl_config::{CompiledCrawlConfig, load_compiled_crawl_config};
use crate::engine::git_ops::resolve_commit_oid;
use crate::engine::identifier::LabelId;
use crate::engine::retrieval::RetrievalMethod;
use crate::engine::storage::{
    CatalogLock, DatabaseLockShared, SOURCE_KIND_GIT_COMMIT, SOURCE_KIND_WORKING_DIRECTORY,
    acquire_catalog_lock, acquire_database_shared,
};

/// Source discriminator for the crawl preamble.
#[derive(Clone, Copy)]
pub(crate) enum CrawlInput<'a> {
    Commit { commit: &'a str },
    WorkingDir,
}

/// Result of the crawl preamble, holding everything the entry points need.
pub(crate) struct CrawlPreamble {
    pub(crate) selection: BTreeSet<RetrievalMethod>,
    pub(crate) total_start: Instant,
    pub(crate) repo_path: PathBuf,
    pub(crate) label_id: LabelId,
    pub(crate) crawl_config: CompiledCrawlConfig,
    pub(crate) db_path: PathBuf,
    pub(crate) source_metadata: CrawlSourceMetadata,

    // Held for RAII. These must remain alive until run_crawl_async returns.
    pub(crate) _db_guard: DatabaseLockShared,
    pub(crate) _catalog_guard: CatalogLock,
}

/// Prepares the shared crawl preamble for both commit and working-directory entry points.
pub(crate) fn prepare_crawl_preamble(
    config: &Config,
    catalog_name: &str,
    label: &str,
    retrieval: Vec<RetrievalMethod>,
    input: CrawlInput<'_>,
) -> Result<CrawlPreamble> {
    let selection = normalize_retrieval(retrieval);
    let total_start = std::time::Instant::now();

    // Print starting banner
    match input {
        CrawlInput::Commit { .. } => println!("🔍 Starting label-aware crawl..."),
        CrawlInput::WorkingDir => println!("🔍 Starting working directory crawl..."),
    }
    println!("Catalog: {}", catalog_name);
    println!(
        "Label: {} {}",
        label,
        format_selection_for_display(&selection)
    );

    // Get catalog config
    let catalog_config = config
        .catalogs
        .get(catalog_name)
        .ok_or_else(|| anyhow!("Catalog '{}' not found in config", catalog_name))?;

    // Validate catalog path (must be absolute, no ~ or $VAR)
    let repo_path = validate_config_path("catalog path", &catalog_config.path)?;
    println!("Repository: {}", repo_path.display());
    println!("Type: {}", catalog_config.r#type);

    // Print source line
    match input {
        CrawlInput::Commit { commit } => println!("Commit: {}", commit),
        CrawlInput::WorkingDir => println!("Source: working directory (uncommitted changes)"),
    }
    println!();

    // Compute label_id (internal storage form)
    let label_id = LabelId::new(catalog_name, label).map_err(|e| anyhow!("{}", e))?;

    // Load repo-specific crawl configuration
    let crawl_config = load_compiled_crawl_config(&config.paths, Some(&repo_path))?;
    println!("Loaded crawl configuration for repository");

    // Resolve database path (needed for locks and metadata)
    let db_path = resolve_database_path(config)?;
    println!("Database: {}", db_path.display());

    // Acquire locks before any catalog-scoped I/O
    // Order: DatabaseLockShared first, then CatalogLock
    let _db_guard = acquire_database_shared(&db_path, &stderr_lock_progress)?;
    let _catalog_guard = acquire_catalog_lock(&db_path, catalog_name, &stderr_lock_progress)?;

    // Construct source_metadata based on input kind
    let source_metadata = match input {
        CrawlInput::Commit { commit } => {
            println!("📦 Resolving commit...");
            let commit_oid = resolve_commit_oid(&repo_path, commit)?;
            println!("Resolved {} to {}", commit, &commit_oid[..12]);
            println!();
            CrawlSourceMetadata {
                source_kind: SOURCE_KIND_GIT_COMMIT,
                source_value: commit_oid,
            }
        }
        CrawlInput::WorkingDir => CrawlSourceMetadata {
            source_kind: SOURCE_KIND_WORKING_DIRECTORY,
            source_value: crate::engine::make_working_dir_source_sentinel(),
        },
    };

    Ok(CrawlPreamble {
        selection,
        total_start,
        repo_path,
        label_id,
        crawl_config,
        db_path,
        source_metadata,
        _db_guard,
        _catalog_guard,
    })
}

/// Returns all retrieval methods (used when no explicit --retrieval is specified).
fn all_retrieval_methods() -> BTreeSet<RetrievalMethod> {
    let mut methods = BTreeSet::new();
    methods.insert(RetrievalMethod::Fts);
    methods.insert(RetrievalMethod::Vector);
    methods
}

/// Normalizes `Vec<RetrievalMethod>` to `BTreeSet<RetrievalMethod>`.
/// Empty vec means all methods; non-empty vec is deduplicated into a set.
fn normalize_retrieval(retrieval: Vec<RetrievalMethod>) -> BTreeSet<RetrievalMethod> {
    if retrieval.is_empty() {
        all_retrieval_methods()
    } else {
        // Set semantics: --retrieval vector --retrieval vector collapses to {Vector}
        retrieval.into_iter().collect()
    }
}

/// Formats the retrieval selection for display in the crawl preamble.
///
/// Returns a string like "(fts, vector)", "(fts only)", "(vector only)",
/// or "(no retrieval methods)" for empty selection.
pub fn format_selection_for_display(selection: &BTreeSet<RetrievalMethod>) -> String {
    format!(
        "({})",
        crate::engine::retrieval::format_selection(selection)
    )
}

/// Prints the selection-narrowing announcement if applicable.
///
/// This should be called after the previous label metadata has been read,
/// which happens after storage is opened. The announcement prints separately
/// from the main preamble (which prints before storage is open).
pub fn print_narrowing_announcement(
    writer: &mut dyn std::io::Write,
    previous_selection: &BTreeSet<RetrievalMethod>,
    new_selection: &BTreeSet<RetrievalMethod>,
) {
    // Only print if this is a strict narrowing (previous is a strict superset of new)
    if previous_selection.is_superset(new_selection) && previous_selection != new_selection {
        let has_fts = new_selection.contains(&RetrievalMethod::Fts);
        let has_vector = new_selection.contains(&RetrievalMethod::Vector);

        match (has_fts, has_vector) {
            (true, false) => {
                writeln!(writer).unwrap();
                writeln!(
                    writer,
                    "👉 This crawl narrows retrieval selection to fts only, no vector."
                )
                .unwrap();
                writeln!(
                    writer,
                    "   Any vector data from a previous crawl is preserved and will be reused"
                )
                .unwrap();
                writeln!(writer, "   if you re-add vector to the selection.").unwrap();
            }
            (false, true) => {
                writeln!(writer).unwrap();
                writeln!(
                    writer,
                    "👉 This crawl narrows retrieval selection to vector only, no fts."
                )
                .unwrap();
                writeln!(
                    writer,
                    "   Any fts data from a previous crawl is preserved and will be reused"
                )
                .unwrap();
                writeln!(writer, "   if you re-add fts to the selection.").unwrap();
            }
            // Empty selection or other narrowing combinations are not expected in practice,
            // but if they occur, we don't print a misleading message.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_selection(methods: &[RetrievalMethod]) -> BTreeSet<RetrievalMethod> {
        methods.iter().cloned().collect()
    }

    #[test]
    fn test_format_selection_both_methods() {
        let selection = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        assert_eq!(format_selection_for_display(&selection), "(fts, vector)");
    }

    #[test]
    fn test_format_selection_fts_only() {
        let selection = make_selection(&[RetrievalMethod::Fts]);
        assert_eq!(format_selection_for_display(&selection), "(fts only)");
    }

    #[test]
    fn test_format_selection_vector_only() {
        let selection = make_selection(&[RetrievalMethod::Vector]);
        assert_eq!(format_selection_for_display(&selection), "(vector only)");
    }

    #[test]
    fn test_format_selection_empty() {
        let selection: BTreeSet<RetrievalMethod> = BTreeSet::new();
        assert_eq!(
            format_selection_for_display(&selection),
            "(no retrieval methods)"
        );
    }

    #[test]
    fn test_narrowing_announcement_fts_only() {
        // Previous: both methods, New: fts only -> should print narrowing announcement
        let previous = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let new = make_selection(&[RetrievalMethod::Fts]);
        let mut output = Vec::new();
        print_narrowing_announcement(&mut output, &previous, &new);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("narrows retrieval selection to fts only"));
        assert!(output.contains("vector data from a previous crawl is preserved"));
    }

    #[test]
    fn test_narrowing_announcement_vector_only() {
        // Previous: both methods, New: vector only -> should print narrowing announcement
        let previous = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let new = make_selection(&[RetrievalMethod::Vector]);
        let mut output = Vec::new();
        print_narrowing_announcement(&mut output, &previous, &new);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("narrows retrieval selection to vector only"));
        assert!(output.contains("fts data from a previous crawl is preserved"));
    }

    #[test]
    fn test_narrowing_announcement_no_narrowing_same() {
        // Previous and new are the same -> no announcement
        let previous = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let new = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let mut output = Vec::new();
        print_narrowing_announcement(&mut output, &previous, &new);
        assert!(output.is_empty());
    }

    #[test]
    fn test_narrowing_announcement_no_narrowing_widening() {
        // Previous: fts only, New: both -> widening, not narrowing
        let previous = make_selection(&[RetrievalMethod::Fts]);
        let new = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let mut output = Vec::new();
        print_narrowing_announcement(&mut output, &previous, &new);
        assert!(output.is_empty());
    }

    #[test]
    fn test_narrowing_announcement_first_crawl() {
        // Previous: empty (first crawl), New: both -> no narrowing announcement
        let previous: BTreeSet<RetrievalMethod> = BTreeSet::new();
        let new = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let mut output = Vec::new();
        print_narrowing_announcement(&mut output, &previous, &new);
        assert!(output.is_empty());
    }
}
