//! Warning rendering for crawl output.
//!
//! Purpose: Render in-flight crawl warnings to stdout/stderr with byte-identical output to pre-FTS behavior.
//! Edit here when: Changing warning message format, adding new warning renderers.
//! Do not edit here for: Warning type definitions (see `engine/warning.rs`), warning emission in phases (see `phases.rs`).

use crate::engine::warning::CrawlWarning;
use std::cell::Cell;
use std::io::Write;

/// Internal helper that renders warnings to injectable writers.
///
/// This enables testing with string buffers while production uses process stdout/stderr.
/// The routing is:
/// - `ChunkerFallbackSplit` → stdout (matches current `println!("Warning: Couldn't find...")`)
/// - All other variants → stderr (matches current `eprintln!` calls)
fn render_warning_to<W1, W2>(warning: &CrawlWarning, stdout: &mut W1, stderr: &mut W2)
where
    W1: Write,
    W2: Write,
{
    match warning {
        CrawlWarning::ChunkerFallbackSplit { relative_path } => {
            // Byte-identical to the current println! in chunk_new_files
            writeln!(
                stdout,
                "Warning: Couldn't find a splitpoint for {}",
                relative_path
            )
            .expect("Failed to write warning to stdout");
        }
        CrawlWarning::FileReadFailed {
            relative_path,
            error,
        } => {
            // Byte-identical to the current eprintln! in chunk_new_files
            writeln!(
                stderr,
                "\n  ⚠️  Failed to read {}: {}",
                relative_path, error
            )
            .expect("Failed to write warning to stderr");
        }
        CrawlWarning::ChunkingFailed {
            relative_path,
            error,
        } => {
            // Byte-identical to the current eprintln! in chunk_new_files
            writeln!(
                stderr,
                "\n  ⚠️  Failed to chunk {}: {}",
                relative_path, error
            )
            .expect("Failed to write warning to stderr");
        }
        CrawlWarning::SentinelReadFailed {
            relative_path,
            error,
        } => {
            // Byte-identical to the current eprintln! in classify_files
            writeln!(
                stderr,
                "  ⚠️  Error checking sentinel for {}: {}",
                relative_path, error
            )
            .expect("Failed to write warning to stderr");
        }
        CrawlWarning::FtsZeroTokens { row_id } => {
            // New in FTS PR1 - will be used in Stage 4
            writeln!(
                stderr,
                "  ⚠️  FTS tokenizer produced zero tokens for chunk {}",
                row_id
            )
            .expect("Failed to write warning to stderr");
        }
    }
}

/// Creates a warning sink closure that renders to process stdout/stderr.
///
/// The closure captures a counter by reference and increments it for `ChunkerFallbackSplit`
/// warnings (matching current behavior where only fallback-split warnings are counted in the
/// in-progress display).
///
/// # Arguments
///
/// * `counter` - Shared counter for fallback-split warnings (used in progress display)
///
/// # Returns
///
/// A closure suitable for use as a `WarningSink` that writes to process stdout/stderr.
pub fn create_warning_sink(counter: &Cell<usize>) -> impl FnMut(CrawlWarning) + '_ {
    move |warning: CrawlWarning| {
        // Increment counter only for ChunkerFallbackSplit (matches current behavior)
        if matches!(warning, CrawlWarning::ChunkerFallbackSplit { .. }) {
            counter.set(counter.get() + 1);
        }

        let mut stdout = std::io::stdout();
        let mut stderr = std::io::stderr();
        render_warning_to(&warning, &mut stdout, &mut stderr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunker_fallback_split_goes_to_stdout() {
        let warning = CrawlWarning::ChunkerFallbackSplit {
            relative_path: "src/example.ts".to_string(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_warning_to(&warning, &mut stdout, &mut stderr);

        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();

        assert_eq!(
            stdout_str,
            "Warning: Couldn't find a splitpoint for src/example.ts\n"
        );
        assert!(stderr_str.is_empty());
    }

    #[test]
    fn test_file_read_failed_goes_to_stderr() {
        let warning = CrawlWarning::FileReadFailed {
            relative_path: "src/example.ts".to_string(),
            error: "permission denied".to_string(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_warning_to(&warning, &mut stdout, &mut stderr);

        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();

        assert!(stdout_str.is_empty());
        assert_eq!(
            stderr_str,
            "\n  ⚠️  Failed to read src/example.ts: permission denied\n"
        );
    }

    #[test]
    fn test_chunking_failed_goes_to_stderr() {
        let warning = CrawlWarning::ChunkingFailed {
            relative_path: "src/example.ts".to_string(),
            error: "parse error".to_string(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_warning_to(&warning, &mut stdout, &mut stderr);

        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();

        assert!(stdout_str.is_empty());
        assert_eq!(
            stderr_str,
            "\n  ⚠️  Failed to chunk src/example.ts: parse error\n"
        );
    }

    #[test]
    fn test_sentinel_read_failed_goes_to_stderr() {
        let warning = CrawlWarning::SentinelReadFailed {
            relative_path: "src/example.ts".to_string(),
            error: "io error".to_string(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_warning_to(&warning, &mut stdout, &mut stderr);

        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();

        assert!(stdout_str.is_empty());
        assert_eq!(
            stderr_str,
            "  ⚠️  Error checking sentinel for src/example.ts: io error\n"
        );
    }

    #[test]
    fn test_fts_zero_tokens_goes_to_stderr() {
        let warning = CrawlWarning::FtsZeroTokens {
            row_id: "abc123:3".to_string(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_warning_to(&warning, &mut stdout, &mut stderr);

        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();

        assert!(stdout_str.is_empty());
        assert_eq!(
            stderr_str,
            "  ⚠️  FTS tokenizer produced zero tokens for chunk abc123:3\n"
        );
    }

    #[test]
    fn test_counter_increments_only_for_fallback_split() {
        use std::cell::Cell;

        let counter = Cell::new(0);
        let mut sink = create_warning_sink(&counter);

        // ChunkerFallbackSplit increments counter
        sink(CrawlWarning::ChunkerFallbackSplit {
            relative_path: "file1.ts".to_string(),
        });
        assert_eq!(counter.get(), 1);

        // Other warnings don't increment
        sink(CrawlWarning::FileReadFailed {
            relative_path: "file2.ts".to_string(),
            error: "error".to_string(),
        });
        assert_eq!(counter.get(), 1);

        sink(CrawlWarning::ChunkerFallbackSplit {
            relative_path: "file3.ts".to_string(),
        });
        assert_eq!(counter.get(), 2);
    }
}
