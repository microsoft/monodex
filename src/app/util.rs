//! Purpose: App-wide formatting and display utilities — timestamps, durations, byte sizes, terminal-output sanitization for `>`-prefixed search lines, chunk selector parsing.
//! Edit here when: Adding or modifying user-facing formatting helpers, or chunk selector parsing logic.
//! Do not edit here for: Engine-wide utilities (see `engine/util.rs`), view command output (see `app/commands/view.rs`).

/// Get current timestamp for logging (HH:MM:SS format)
/// Format current time as HH:MM:SS for progress-log use.
pub fn log_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let h = (now / 3600) % 24;
    let m = (now / 60) % 60;
    let s = now % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Format current time as UTC RFC 3339 string (e.g., "2024-01-15T10:30:00Z").
/// Used for machine-readable timestamps like the context file's `set_at` field.
pub fn utc_rfc3339_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Convert to calendar time (UTC)
    let days = now / 86400;
    let secs_today = now % 86400;

    // Calculate date from days since 1970-01-01
    // Using the algorithm from: https://en.wikipedia.org/wiki/Julian_day
    let (year, month, day) = days_to_ymd(days as i64);

    let hour = (secs_today / 3600) as u8;
    let minute = ((secs_today % 3600) / 60) as u8;
    let second = (secs_today % 60) as u8;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

/// Convert days since 1970-01-01 to year, month, day.
/// Based on the Julian day algorithm.
fn days_to_ymd(days: i64) -> (i16, u8, u8) {
    // Julian day number for 1970-01-01 is 2440588
    let jd = days + 2440588;

    // Algorithm from Richards (2012)
    let f = jd + 1401 + (((4 * jd + 274277) / 146097) * 3) / 4 - 38;
    let e = 4 * f + 3;
    let g = (e % 1461) / 4;
    let h = 5 * g + 2;
    let day = ((h % 153) / 5) + 1;
    let month = ((h / 153 + 2) % 12) + 1;
    let year = e / 1461 - 4716 + (12 + 2 - month) / 12;

    (year as i16, month as u8, day as u8)
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

/// Format an integer count with comma thousands separators.
///
/// Examples: `24396 -> "24,396"`, `4863 -> "4,863"`, `999 -> "999"`
///
/// Used for counts of things the user is processing (chunks, files, tokens, labels, warnings).
/// Not for identifiers, ordinals, fixed parameters, or values that are always small.
pub fn format_count(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }

    let digits: Vec<char> = n.to_string().chars().collect();
    let mut result = String::with_capacity(digits.len() + (digits.len() - 1) / 3);

    let first_group_len = digits.len() % 3;
    if first_group_len > 0 {
        for ch in &digits[..first_group_len] {
            result.push(*ch);
        }
        result.push(',');
    }

    let remaining = if first_group_len > 0 {
        &digits[first_group_len..]
    } else {
        &digits
    };

    for (i, ch) in remaining.iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(*ch);
    }

    result
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
    use crate::engine::storage::{SOURCE_KIND_GIT_COMMIT, SOURCE_KIND_WORKING_DIRECTORY};

    match row.source_kind.as_str() {
        SOURCE_KIND_GIT_COMMIT => row
            .vector_source
            .as_ref()
            .or(row.fts_source.as_ref())
            .map(|s| format!("--commit {}", s))
            .unwrap_or_else(|| "--commit <commit>".to_string()),
        SOURCE_KIND_WORKING_DIRECTORY => "--working-dir".to_string(),
        _ => "[source]".to_string(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // parse_chunk_selector tests
    // =========================================================================

    #[test]
    fn test_parse_file_id_all_chunks() {
        let (file_id, selector) = parse_chunk_selector("abcd1234efab5678").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::All));
    }

    #[test]
    fn test_parse_file_id_single_chunk() {
        let (file_id, selector) = parse_chunk_selector("abcd1234efab5678:3").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::Single(3)));
    }

    #[test]
    fn test_parse_file_id_range() {
        let (file_id, selector) = parse_chunk_selector("abcd1234efab5678:2-4").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::Range(2, 4)));
    }

    #[test]
    fn test_parse_file_id_to_end() {
        let (file_id, selector) = parse_chunk_selector("abcd1234efab5678:3-end").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::ToEnd(3)));
    }

    #[test]
    fn test_parse_file_id_invalid_file_id() {
        let result = parse_chunk_selector("invalid");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid file ID"));
    }

    #[test]
    fn test_parse_file_id_invalid_selector() {
        let result = parse_chunk_selector("abcd1234efab5678:abc");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid chunk number")
        );
    }

    #[test]
    fn test_parse_file_id_end_without_start() {
        let result = parse_chunk_selector("abcd1234efab5678:end");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid selector ':end'")
        );
    }

    #[test]
    fn test_parse_file_id_zero_chunk_number() {
        let result = parse_chunk_selector("abcd1234efab5678:0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("1-indexed"));
    }

    #[test]
    fn test_parse_file_id_reversed_range() {
        let result = parse_chunk_selector("abcd1234efab5678:5-2");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Start chunk 5 > end chunk 2")
        );
    }

    // =========================================================================
    // format_count tests
    // =========================================================================

    #[test]
    fn test_format_count_under_1000() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(1), "1");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn test_format_count_exactly_1000() {
        assert_eq!(format_count(1000), "1,000");
    }

    #[test]
    fn test_format_count_multi_comma() {
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(1000000), "1,000,000");
        assert_eq!(format_count(999999999), "999,999,999");
    }

    #[test]
    fn test_format_count_various_values() {
        assert_eq!(format_count(4863), "4,863");
        assert_eq!(format_count(24396), "24,396");
        assert_eq!(format_count(2254), "2,254");
    }
}
