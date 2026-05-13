//! Purpose: Handler for the `audit-chunks` command — sample TypeScript files and report aggregate chunk-quality scores.
//! Edit here when: Modifying chunk-quality auditing output, sampling, or AST-only reporting.
//! Do not edit here for: Chunking algorithm (see `engine/partitioner/`).

use anyhow::Result;
use std::path::PathBuf;

use crate::app::format_count;
use crate::engine::partitioner::{ChunkQualityReport, PartitionConfig, partition_typescript};

pub fn run_audit_chunks(count: usize, dir: String) -> Result<()> {
    use rand::seq::IndexedRandom;

    println!(
        "📊 Sampling {} TypeScript files from: {}",
        format_count(count as u64),
        dir
    );
    println!();

    // Collect all TypeScript files
    let ts_files: Vec<PathBuf> = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            let ext = path
                .extension()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default();
            (ext == "ts" || ext == "tsx") && !path.to_string_lossy().contains("node_modules")
        })
        .map(|e| e.path().to_owned())
        .collect();

    println!(
        "Found {} TypeScript files",
        format_count(ts_files.len() as u64)
    );

    if ts_files.is_empty() {
        return Err(anyhow::anyhow!("No TypeScript files found"));
    }

    // Random sample
    let mut rng = rand::rng();
    let sample: Vec<_> = ts_files.sample(&mut rng, count).collect();

    // Compute quality scores using AST-only mode (allow_fallback=false)
    // This measures how well the AST-based chunker performs, without fallback
    // masking the quality of split decisions.
    let mut results: Vec<_> = sample
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(path).ok()?;
            let file_name = path.file_name()?.to_string_lossy().to_string();
            let config = PartitionConfig {
                file_name,
                package_name: "n/a".to_string(),
                allow_fallback: false, // AST-only mode for accurate quality measurement
                ..Default::default()
            };
            let chunks =
                partition_typescript(&source, &config, path.to_str().unwrap(), "n/a").ok()?;
            let file_chars = source.len();
            let report = ChunkQualityReport::from_chunks(&chunks, file_chars);
            Some((path, report, chunks))
        })
        .collect();

    // Sort by score (worst first - ascending since higher is better)
    results.sort_by(|a, b| a.1.score.partial_cmp(&b.1.score).unwrap());

    println!("\n=== Quality Scores (worst first) ===\n");
    for (i, (path, report, _)) in results.iter().enumerate() {
        let rel_path = path.strip_prefix(&dir).unwrap_or(path);
        println!("{}. {} {}", i + 1, report.format(), rel_path.display());
    }

    // Show top 3 worst for investigation
    println!("\n=== Top 3 Worst Files ===\n");
    for (path, report, chunks) in results.iter().take(3) {
        let rel_path = path.strip_prefix(&dir).unwrap_or(path);
        println!("--- {} ---", rel_path.display());
        println!("{}", report.format());

        // Show chunk breakdown
        for (i, chunk) in chunks.iter().enumerate() {
            let lines = chunk.end_line - chunk.start_line + 1;
            let tiny_marker = if lines < 20 { " [TINY]" } else { "" };
            println!(
                "  Chunk {}: {} lines ({}-{}) {} - {}",
                i + 1,
                lines,
                chunk.start_line,
                chunk.end_line,
                tiny_marker,
                chunk.breadcrumb
            );
        }
        println!();
    }

    Ok(())
}
