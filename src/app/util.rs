//! App-wide utility functions for formatting and display.
//!
//! Edit here when: Adding formatting helpers, timestamp utilities,
//! or terminal output functions.
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
