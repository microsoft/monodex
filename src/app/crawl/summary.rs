//! Crawl completion and warning summary rendering.
//!
//! Purpose: Render the final crawl summary and warning summaries after
//!   all phases have completed.
//!
//! Edit here when: Changing how crawl results are displayed to users,
//!   adding new summary metrics, or modifying warning output format.
//! Do not edit here for: Crawl phase logic (see phases.rs), pipeline
//!   execution (see pipeline.rs), command parsing (see commands/crawl.rs).

use std::collections::HashSet;

use crate::app::{format_count, format_duration};

fn write_summary_counts(
    out: &mut impl std::io::Write,
    total_elapsed: std::time::Duration,
    new_count: usize,
    existing_count: usize,
    existing_success_count: usize,
) {
    writeln!(
        out,
        "  Total time: {}",
        format_duration(total_elapsed.as_secs_f64())
    )
    .unwrap();
    writeln!(
        out,
        "  New files indexed: {}",
        format_count(new_count as u64)
    )
    .unwrap();
    writeln!(
        out,
        "  Existing files detected: {}",
        format_count(existing_count as u64)
    )
    .unwrap();
    writeln!(
        out,
        "  Existing files updated successfully: {}",
        format_count(existing_success_count as u64)
    )
    .unwrap();
}

/// Writes the crawl summary to the given writer.
///
/// This is the core implementation that can be used with any `Write` sink.
/// The `print_summary` function wraps this with stdout.
#[allow(clippy::too_many_arguments)]
pub fn write_summary(
    mut out: impl std::io::Write,
    total_start: std::time::Instant,
    new_count: usize,
    existing_count: usize,
    existing_success_count: usize,
    had_failures: bool,
    cleanup_failed: bool,
    existing_file_failures_count: usize,
    pipeline_failures_count: usize,
    // Phase failure indicators for the summary
    vector_phase_failed: bool,
    fts_phase_failed: bool,
) {
    let total_elapsed = total_start.elapsed();
    if had_failures || cleanup_failed || vector_phase_failed || fts_phase_failed {
        writeln!(out, "⚠️  Crawl completed with errors!").unwrap();
        write_summary_counts(
            &mut out,
            total_elapsed,
            new_count,
            existing_count,
            existing_success_count,
        );
        let total_failures = pipeline_failures_count + existing_file_failures_count;
        writeln!(
            out,
            "  Total failures: {}",
            format_count(total_failures as u64)
        )
        .unwrap();
        if existing_file_failures_count > 0 {
            writeln!(
                out,
                "  - Existing file label-add failures: {}",
                format_count(existing_file_failures_count as u64)
            )
            .unwrap();
        }
        if cleanup_failed {
            writeln!(out, "  - Label cleanup failed (crawl not marked complete)").unwrap();
        }
        if vector_phase_failed {
            writeln!(out, "  - Vector phase: failed (see error above)").unwrap();
        }
        if fts_phase_failed {
            writeln!(out, "  - FTS phase: failed (see error above)").unwrap();
        }
        writeln!(out).unwrap();
        writeln!(
            out,
            "  This crawl is marked as incomplete. Re-run to complete indexing."
        )
        .unwrap();
    } else {
        writeln!(out, "✅ Crawl complete!").unwrap();
        write_summary_counts(
            &mut out,
            total_elapsed,
            new_count,
            existing_count,
            existing_success_count,
        );
    }
}

/// Prints the crawl summary to stdout.
///
/// Wrapper around `write_summary` that writes to stdout.
#[allow(clippy::too_many_arguments)]
pub fn print_summary(
    total_start: std::time::Instant,
    new_count: usize,
    existing_count: usize,
    existing_success_count: usize,
    had_failures: bool,
    cleanup_failed: bool,
    existing_file_failures_count: usize,
    pipeline_failures_count: usize,
    vector_phase_failed: bool,
    fts_phase_failed: bool,
) {
    write_summary(
        std::io::stdout().lock(),
        total_start,
        new_count,
        existing_count,
        existing_success_count,
        had_failures,
        cleanup_failed,
        existing_file_failures_count,
        pipeline_failures_count,
        vector_phase_failed,
        fts_phase_failed,
    )
}

/// Prints the warning summary.
pub fn print_warning_summary(crawl_warning_files: &HashSet<String>) {
    if crawl_warning_files.is_empty() {
        return;
    }
    let mut sorted: Vec<&String> = crawl_warning_files.iter().collect();
    sorted.sort();
    let plural = if sorted.len() == 1 { "file" } else { "files" };
    println!();
    println!(
        "Chunking warnings in {} {}:",
        format_count(sorted.len() as u64),
        plural
    );
    for file in sorted.iter().take(20) {
        println!("  - {}", file);
    }
    if sorted.len() > 20 {
        println!(
            "  ... and {} more",
            format_count((sorted.len() - 20) as u64)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that write_summary includes FTS phase failure in output.
    ///
    /// This verifies the FTS-phase failure is mentioned in the summary output.
    #[test]
    fn test_summary_includes_fts_phase_failure() {
        let mut output = Vec::new();
        let start = std::time::Instant::now();

        write_summary(
            &mut output,
            start,
            10,    // new_count
            5,     // existing_count
            5,     // existing_success_count
            false, // had_failures
            false, // cleanup_failed
            0,     // existing_file_failures_count
            0,     // pipeline_failures_count
            false, // vector_phase_failed
            true,  // fts_phase_failed
        );

        let output_str = String::from_utf8(output).unwrap();

        // Check that the output contains both "FTS" and "failed"
        assert!(
            output_str.contains("FTS"),
            "Summary should mention FTS, got: {}",
            output_str
        );
        assert!(
            output_str.contains("failed"),
            "Summary should mention failure, got: {}",
            output_str
        );
    }
}
