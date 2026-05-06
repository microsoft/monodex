//! Placeholder for the debug-fts command.
//!
//! Purpose: Diagnose FTS tokenization and ranking for a single chunk.
//! Edit here when: Implementing FTS diagnostics (Stage 8).
//! Do not edit here for: FTS engine logic (see engine/fts/).

use anyhow::Result;

/// Placeholder implementation for debug-fts command.
/// Returns Ok(()) printing "not yet implemented".
/// Body is replaced in Stage 8.
pub fn run_debug_fts(
    _id: &str,
    _label: Option<&str>,
    _catalog: Option<&str>,
    _query: Option<&str>,
    _config_path: Option<&std::path::PathBuf>,
    _debug: bool,
) -> Result<()> {
    println!("not yet implemented");
    Ok(())
}
