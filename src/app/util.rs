//! Purpose: App-wide formatting and display utilities — timestamps, durations, byte sizes, terminal-output sanitization for `>`-prefixed search lines.
//! Edit here when: Adding or modifying user-facing formatting helpers.
//! Do not edit here for: Engine-wide utilities (see `engine/util.rs`).

use std::collections::HashSet;

/// Get current timestamp for logging (HH:MM:SS format)
pub fn chrono_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let h = (now / 3600) % 24;
    let m = (now / 60) % 60;
    let s = now % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Format duration in seconds to human-readable string (e.g., "1h 23m" or "5m 30s")
pub fn format_duration(secs: f64) -> String {
    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let s = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else if mins > 0 {
        format!("{}m {}s", mins, s)
    } else {
        format!("{}s", s)
    }
}

/// Format ETA in seconds to human-readable string
pub fn format_eta(secs: f64) -> String {
    if secs <= 0.0 || !secs.is_finite() {
        return "--".to_string();
    }
    format_duration(secs)
}

/// E.1: Sanitize a string for safe terminal output by stripping control characters.
/// This prevents terminal injection attacks from malicious file paths, breadcrumbs, etc.
pub fn sanitize_for_terminal(s: &str) -> String {
    s.chars()
        .filter(|c| {
            // Allow printable ASCII and common Unicode, but strip control characters
            // Control characters are those with code points < 0x20 (space) and DEL (0x7F)
            // Also strip ANSI escape sequences which start with ESC (0x1B)
            !c.is_control() || *c == '\t' || *c == '\n' || *c == '\r'
        })
        .collect()
}

/// Format a chunk's breadcrumb, split-part metadata, and chunk-kind decoration.
///
/// Used by search and view commands to produce consistent chunk report headers.
///
/// - `breadcrumb: None` renders as `"unknown"`
/// - The breadcrumb is sanitized internally for terminal safety
/// - `split_part: Some((ordinal, count))` appends ` (part {ordinal}/{count})`
/// - `chunk_kind != "content"` appends ` [{chunk_kind}]`
pub fn format_chunk_report(
    breadcrumb: Option<&str>,
    split_part: Option<(i32, i32)>,
    chunk_kind: &str,
) -> String {
    let breadcrumb = sanitize_for_terminal(breadcrumb.unwrap_or("unknown"));

    let mut report = breadcrumb;
    if let Some((ordinal, count)) = split_part {
        report = format!("{} (part {}/{})", report, ordinal, count);
    }
    if chunk_kind != "content" {
        report = format!("{} [{}]", report, chunk_kind);
    }

    report
}

// ============================================================================
// Chunking Warning State Persistence
// ============================================================================

/// Get the path to the warning state file for a catalog.
/// Path: <db_root>/warnings-<catalog>.json
pub fn get_warning_state_path(db_root: &std::path::Path, catalog_name: &str) -> std::path::PathBuf {
    db_root.join(format!("warnings-{}.json", catalog_name))
}

/// Load persisted chunking warning files for a catalog.
/// Returns a HashSet of relative paths that had chunking warnings.
pub fn load_warning_state(db_root: &std::path::Path, catalog_name: &str) -> HashSet<String> {
    let path = get_warning_state_path(db_root, catalog_name);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashSet::new(),
    }
}

/// Save chunking warning files for a catalog.
/// Persists the sorted list of relative paths to <db_root>/warnings-<catalog>.json
pub fn save_warning_state(
    db_root: &std::path::Path,
    catalog_name: &str,
    warning_files: &[String],
) -> anyhow::Result<()> {
    let path = get_warning_state_path(db_root, catalog_name);

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(warning_files)?;
    std::fs::write(&path, json)?;
    Ok(())
}

// ============================================================================
// Lock Progress Callback
// ============================================================================

/// Progress callback for lock acquisitions that writes to stderr.
///
/// This is the shared progress callback for database, catalog, and other lock
/// acquisitions across init-db, crawl, and purge commands.
pub fn stderr_lock_progress(msg: &str) {
    eprintln!("{}", msg);
}

// ============================================================================
// Chunk Selector Parsing
// ============================================================================

/// Parsed selector for file-based chunk queries.
///
/// Used by `view` and `debug-fts` commands to parse chunk identifiers
/// like `700a4ba232fe9ddc:3` or `700a4ba232fe9ddc:2-4`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkSelector {
    /// All chunks in the file (no selector suffix)
    All,
    /// Single chunk at position N (1-indexed)
    Single(usize),
    /// Range from start to end (inclusive, 1-indexed)
    Range(usize, usize),
    /// Range from start to the end of file
    ToEnd(usize),
}

/// Parse file ID with optional selector suffix.
///
/// Formats:
/// - `700a4ba232fe9ddc` - all chunks in file (`ChunkSelector::All`)
/// - `700a4ba232fe9ddc:3` - chunk 3 (`ChunkSelector::Single(3)`)
/// - `700a4ba232fe9ddc:2-3` - chunks 2 through 3 (`ChunkSelector::Range(2, 3)`)
/// - `700a4ba232fe9ddc:3-end` - chunk 3 through the last chunk (`ChunkSelector::ToEnd(3)`)
///
/// Returns a tuple of `(file_id, selector)`. The file_id is validated to be
/// exactly 16 hexadecimal characters.
pub fn parse_chunk_selector(s: &str) -> anyhow::Result<(String, ChunkSelector)> {
    let s = s.trim();

    // Check for selector suffix
    if let Some(colon_pos) = s.find(':') {
        let file_id = s[..colon_pos].to_string();
        let selector = &s[colon_pos + 1..];

        // Validate file_id is 16 hex chars
        if file_id.len() != 16 || !file_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                file_id
            ));
        }

        // Parse selector
        if selector == "end" {
            // Invalid: ":end" without start
            Err(anyhow::anyhow!(
                "Invalid selector ':end'. Use ':N-end' format."
            ))
        } else if let Some(start_str) = selector.strip_suffix("-end") {
            // :N-end format
            let start: usize = start_str
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if start < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    start
                ));
            }
            Ok((file_id, ChunkSelector::ToEnd(start)))
        } else if selector.contains('-') {
            // :N-M format
            let parts: Vec<&str> = selector.split('-').collect();
            if parts.len() != 2 {
                return Err(anyhow::anyhow!(
                    "Invalid selector '{}'. Expected ':N-M' format.",
                    selector
                ));
            }
            let start: usize = parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid start chunk in selector '{}'", selector))?;
            let end: usize = parts[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid end chunk in selector '{}'", selector))?;
            if start < 1 || end < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}:{}",
                    start,
                    end
                ));
            }
            if start > end {
                return Err(anyhow::anyhow!("Start chunk {} > end chunk {}", start, end));
            }
            Ok((file_id, ChunkSelector::Range(start, end)))
        } else {
            // :N format (single chunk)
            let chunk_num: usize = selector
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if chunk_num < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    chunk_num
                ));
            }
            Ok((file_id, ChunkSelector::Single(chunk_num)))
        }
    } else {
        // No selector - validate file_id and return All selector
        if s.len() != 16 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                s
            ));
        }
        Ok((s.to_string(), ChunkSelector::All))
    }
}

// ============================================================================
// Source Pointer Formatting
// ============================================================================

/// Format source pointer for remediation messages.
///
/// Produces a `--commit <oid>` or `--working-dir` argument string suitable for
/// suggested crawl commands in error/warning messages.
pub fn format_source_pointer(row: &crate::engine::storage::LabelMetadataRow) -> String {
    match row.source_kind.as_str() {
        "git-commit" => row
            .vector_source
            .as_ref()
            .or(row.fts_source.as_ref())
            .map(|s| format!("--commit {}", s))
            .unwrap_or_else(|| "--commit <commit>".to_string()),
        "working-directory" => "--working-dir".to_string(),
        _ => "[source]".to_string(),
    }
}
