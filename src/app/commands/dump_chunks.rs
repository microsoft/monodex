//! Purpose: Handler for the `dump-chunks` command — visualize partitioner output for a single file.
//! Edit here when: Modifying chunk visualization, debug/visualize/with-fallback modes, or quality reporting in the command.
//! Do not edit here for: Chunking algorithm (see `engine/partitioner/`), package-name fallback (see `engine/package_lookup.rs`).

use std::path::PathBuf;

use crate::app::format_count;
use crate::engine::SMALL_CHUNK_CHARS;
use crate::engine::partitioner::{
    ChunkQualityReport, PartitionConfig, PartitionDebug, partition_typescript,
};

/// Run chunking diagnostics on a TypeScript file
pub fn run_dump_chunks(
    file: &PathBuf,
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
    let package_name = crate::engine::package_lookup::find_package_name(&file_path, "");

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
    let chunks = partition_typescript(&source, &config, &file_path, &package_name);

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
            } else if text_size < 200 {
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
            } else if text_size < 200 {
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
        println!(
            "Average size: {:.0} chars",
            total_chars as f64 / chunks.len() as f64
        );
        println!("Oversized chunks (>{}): {}", target_size, oversized);
        println!("Small chunks (<200): {}", undersized);
        println!("Quality score: {:.1}%", report.score);
        println!(
            "  Small chunks (<{} chars): {}",
            SMALL_CHUNK_CHARS, report.small_chunks
        );
    }

    Ok(())
}
