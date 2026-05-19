//! Purpose: Handler for the `dump-chunks` command — visualize partitioner output for a single file.
//! Edit here when: Modifying chunk visualization, debug/visualize/with-fallback modes, or quality reporting in the command.
//! Do not edit here for: Chunking algorithm (see `engine/partitioner/`).

use std::path::Path;

use crate::app::number_format::format_count;
use crate::engine::SMALL_CHUNK_CHARS;
use crate::engine::git_ops::extract_package_name_from_bytes;
use crate::engine::partitioner::{
    ChunkQualityReport, PartitionConfig, PartitionDebug, partition_typescript,
};

/// Run chunking diagnostics on a TypeScript file
pub fn run_dump_chunks(
    file: &Path,
    target_size: usize,
    visualize: bool,
    with_fallback: bool,
    enable_debug: bool,
) -> anyhow::Result<()> {
    println!("📦 Chunks for: {}", file.display());
    if !with_fallback {
        println!("🔍 Strict mode: AST-only (fallback disabled)");
    }
    println!();

    // Read file
    let source =
        std::fs::read_to_string(file).map_err(|e| anyhow::anyhow!("Failed to read file: {}", e))?;

    // Determine file name and package name
    let file_name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown.ts");

    // Find package name by walking upward to find nearest package.json
    let file_path = file.to_string_lossy().to_string();
    let package_name = find_package_name(&file_path, "");

    // Create config
    let config = PartitionConfig {
        target_size,
        file_name: file_name.to_string(),
        package_name: package_name.clone(),
        debug: PartitionDebug {
            enabled: enable_debug,
        },
        allow_fallback: with_fallback, // AST-only by default, enable fallback with flag
    };

    // Partition
    let chunks = partition_typescript(&source, &config, &file_path, &package_name)
        .map_err(|e| anyhow::anyhow!("Partitioning failed: {}", e))?;

    // Quality score
    let file_chars = source.len();
    let report = ChunkQualityReport::from_chunks(&chunks, file_chars);

    if visualize {
        // Visualization mode: show full chunk contents
        let lines: Vec<&str> = source.lines().collect();

        for (i, chunk) in chunks.iter().enumerate() {
            let line_count = chunk.end_line - chunk.start_line + 1;
            let size = chunk.text.len();

            println!(
                "-- [CHUNK {}] [{} lines] [{} chars] --",
                i + 1,
                line_count,
                size
            );

            for line_num in chunk.start_line..=chunk.end_line {
                if line_num > 0 && line_num <= lines.len() {
                    println!("{}", lines[line_num - 1]);
                }
            }
            println!();
        }

        println!("=== QUALITY SCORE ===");
        println!("Score: {:.1}%", report.score);
        println!("Total chunks: {}", format_count(chunks.len() as u64));
        println!(
            "Small chunks (<{} chars): {}",
            SMALL_CHUNK_CHARS, report.small_chunks
        );
        println!(
            "Chars: {}-{} (mean {:.0})",
            report.min_chars, report.max_chars, report.mean_chars
        );
    } else {
        // Default mode: show summary with previews
        println!("Total chunks: {}", format_count(chunks.len() as u64));
        println!("Target size: {} chars", target_size);
        println!();

        let mut total_chars = 0;
        let mut oversized = 0;
        let mut undersized = 0;

        for (i, chunk) in chunks.iter().enumerate() {
            let text_size = chunk.text.len();
            let total_size = chunk.breadcrumb.len() + chunk.text.len();
            total_chars += total_size;

            if text_size > target_size {
                oversized += 1;
            } else if text_size < SMALL_CHUNK_CHARS {
                undersized += 1;
            }

            println!("━━━━━ Chunk {} ━━━━━", i + 1);
            println!("Breadcrumb: {}", chunk.breadcrumb);
            println!("Type: {}", chunk.chunk_type);
            if let Some(symbol) = &chunk.symbol_name {
                println!("Symbol: {}", symbol);
            }
            println!("Lines: {}-{}", chunk.start_line, chunk.end_line);
            println!(
                "Size: {} chars (text: {}, breadcrumb: {})",
                total_size,
                text_size,
                chunk.breadcrumb.len()
            );
            if text_size > target_size {
                println!(
                    "⚠️  OVERSIZED (target: {}, actual: {})",
                    target_size, text_size
                );
            } else if text_size < SMALL_CHUNK_CHARS {
                println!("⚡ Small chunk");
            }
            println!();
            println!("Preview (first 8 lines):");
            for line in chunk.text.lines().take(8) {
                println!("  {}", line);
            }
            if chunk.text.lines().count() > 8 {
                println!("  ... ({} more lines)", chunk.text.lines().count() - 8);
            }
            println!();
        }

        println!("━━━━━ Summary ━━━━━");
        println!("Total chunks: {}", format_count(chunks.len() as u64));
        println!("Total chars: {}", total_chars);
        if chunks.is_empty() {
            println!("Average size: (no chunks)");
        } else {
            println!(
                "Average size: {:.0} chars",
                total_chars as f64 / chunks.len() as f64
            );
        }
        println!("Oversized chunks (>{}): {}", target_size, oversized);
        println!("Small chunks (<{}): {}", SMALL_CHUNK_CHARS, undersized);
        println!("Quality score: {:.1}%", report.score);
        println!(
            "  Small chunks (<{} chars): {}",
            SMALL_CHUNK_CHARS, report.small_chunks
        );
    }

    Ok(())
}

// =============================================================================
// Package-name fallback (folded from engine/package_lookup.rs)
//
// Filesystem-only package-name lookup that walks up to find the nearest
// `package.json`. Used only by this command.
// =============================================================================

/// Find the package name for a given source file.
///
/// This walks upwards from the file's directory to find the nearest package.json
/// and extracts the "name" field. If no package.json is found, it uses
/// the relative folder path from the repo root as a fallback identifier.
///
/// # Arguments
///
/// * `file_path` - Path to a source file
/// * `repo_root` - Root of the monorepo (for fallback path generation)
///
/// # Returns
///
/// Package name string (either from package.json or derived from folder structure)
fn find_package_name(file_path: &str, repo_root: &str) -> String {
    let path = Path::new(file_path);

    // Start from the file's directory
    let mut current = path.parent().unwrap_or(path);

    // Walk upwards looking for package.json
    loop {
        let package_json = current.join("package.json");

        if package_json.exists() {
            // Found package.json - try to read and parse it
            if let Some(name) = extract_package_name(&package_json) {
                return name;
            }
            // package.json exists but couldn't parse - keep walking up
        }

        // Go to parent
        match current.parent() {
            Some(parent) => current = parent,
            None => break, // Reached root
        }
    }

    // No package.json found - use relative folder path as identifier
    // e.g., "/repo/libs/util/src/helper.ts" -> "libs/util/src"
    strip_to_relative_path(file_path, repo_root)
}

/// Extracts the "name" field from a package.json file.
///
/// Delegates to the shared `extract_package_name_from_bytes` which uses
/// proper JSON parsing (not string search) to handle edge cases.
fn extract_package_name(package_json: &Path) -> Option<String> {
    let content = std::fs::read(package_json).ok()?;
    extract_package_name_from_bytes(&content)
}

/// Converts an absolute path to a relative path from the repo root.
///
/// For files not in a package, uses the folder structure as the identifier.
/// e.g., "/repo/libs/util/src/file.ts" -> "libs/util/src"
fn strip_to_relative_path(file_path: &str, repo_root: &str) -> String {
    let repo_path = Path::new(repo_root);
    let file_path = Path::new(file_path);

    // Try to strip the repo root
    if let Ok(rel) = file_path.strip_prefix(repo_path) {
        // Get the directory part only (remove the filename)
        let dir = rel.parent().unwrap_or(rel);
        // Convert to string, replace backslashes with forward slashes
        dir.to_string_lossy().replace('\\', "/")
    } else {
        // Couldn't strip - use just the folder name
        file_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_name_from_nonexistent_file() {
        // This test just verifies the function doesn't crash
        let result = extract_package_name(Path::new("/nonexistent/package.json"));
        assert!(result.is_none());
    }
}
