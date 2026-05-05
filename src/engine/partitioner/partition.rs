//! Purpose: Top-level entry point for TypeScript/TSX partitioning — parse with tree-sitter, drive the recursive split search, finalize chunks with metadata.
//! Edit here when: Modifying the top-level partition flow, parser setup, or chunk finalization.
//! Do not edit here for: Split-point search (see `split_search.rs`), AST node analysis (see `node_analysis.rs`), chunk types (see `types.rs`).

use crate::engine::breadcrumb::encode_path_component;
use crate::engine::util::compute_hash;
use tree_sitter::{Language, Parser};

use super::node_analysis::{extract_imports_end_line, get_chunk_metadata, get_lines_text};
use super::split_search::find_best_split;
use super::types::{ChunkRange, MIN_CHUNK_RATIO, PartitionConfig, PartitionedChunk, SplitResult};

/// Partition a TypeScript/TSX file into chunks
pub fn partition_typescript(
    source: &str,
    config: &PartitionConfig,
    file_path: &str,
    catalog: &str,
) -> Vec<PartitionedChunk> {
    let content_hash = compute_hash(source);

    let mut parser = Parser::new();

    // Use TSX grammar for .tsx files, TypeScript grammar for .ts files
    let is_tsx = file_path.ends_with(".tsx");
    if is_tsx {
        parser
            .set_language(&Language::from(tree_sitter_typescript::LANGUAGE_TSX))
            .expect("Failed to set TSX language");
    } else {
        parser
            .set_language(&Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT))
            .expect("Failed to set TypeScript language");
    }

    let tree = parser
        .parse(source, None)
        .expect("Failed to parse TypeScript");

    let root = tree.root_node();
    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len();

    // Build base breadcrumb: package:file
    // Both package name and file name are percent-encoded to handle reserved characters
    let encoded_package = encode_path_component(&config.package_name);
    let encoded_file_name = encode_path_component(&config.file_name);
    let base_breadcrumb = if encoded_package.is_empty() {
        encoded_file_name
    } else {
        format!("{}:{}", encoded_package, encoded_file_name)
    };

    // Step 1: Start with the whole file as one chunk
    let mut chunks: Vec<ChunkRange> = vec![ChunkRange {
        start_line: 1,
        end_line: total_lines,
        from_fallback: false,
        from_degraded_ast_split: false,
    }];

    // Also extract imports end line for chunk_kind metadata (but don't pre-split)
    let import_end_line = extract_imports_end_line(root, source.as_bytes());

    // Step 2: Iteratively split chunks that exceed budget
    let min_chunk_size = (config.target_size as f64 * MIN_CHUNK_RATIO) as usize;
    let mut changed = true;
    while changed {
        changed = false;
        let mut new_chunks = Vec::new();

        for chunk_range in &chunks {
            let chunk_text = get_lines_text(&lines, chunk_range.start_line, chunk_range.end_line);
            let chunk_size = chunk_text.len();

            if chunk_size <= config.target_size {
                new_chunks.push(chunk_range.clone());
            } else {
                // Chunk is too big - use scope-based splitting
                match find_best_split(
                    root,
                    chunk_range.start_line,
                    chunk_range.end_line,
                    chunk_size,
                    config.target_size,
                    min_chunk_size,
                    source.as_bytes(),
                    &config.debug,
                ) {
                    SplitResult::Split(split_line) => {
                        new_chunks.push(ChunkRange {
                            start_line: chunk_range.start_line,
                            end_line: split_line,
                            from_fallback: false,
                            from_degraded_ast_split: false,
                        });
                        new_chunks.push(ChunkRange {
                            start_line: split_line + 1,
                            end_line: chunk_range.end_line,
                            from_fallback: false,
                            from_degraded_ast_split: false,
                        });
                        changed = true;
                    }
                    SplitResult::DegradedSplit(split_line) => {
                        // Degraded AST split: semantically meaningful but poor geometry
                        // Mark as degraded for visibility in diagnostics
                        new_chunks.push(ChunkRange {
                            start_line: chunk_range.start_line,
                            end_line: split_line,
                            from_fallback: false,
                            from_degraded_ast_split: true,
                        });
                        new_chunks.push(ChunkRange {
                            start_line: split_line + 1,
                            end_line: chunk_range.end_line,
                            from_fallback: false,
                            from_degraded_ast_split: true,
                        });
                        changed = true;
                    }
                    SplitResult::Fallback(split_line) => {
                        if config.allow_fallback {
                            new_chunks.push(ChunkRange {
                                start_line: chunk_range.start_line,
                                end_line: split_line,
                                from_fallback: true,
                                from_degraded_ast_split: false,
                            });
                            new_chunks.push(ChunkRange {
                                start_line: split_line + 1,
                                end_line: chunk_range.end_line,
                                from_fallback: true,
                                from_degraded_ast_split: false,
                            });
                            changed = true;
                        } else {
                            // In strict mode, leave oversized chunks as-is
                            new_chunks.push(chunk_range.clone());
                        }
                    }
                    SplitResult::CannotSplit => {
                        new_chunks.push(chunk_range.clone());
                    }
                }
            }
        }
        chunks = new_chunks;
    }

    // Step 3: Convert chunk ranges to PartitionedChunks
    let mut result = Vec::new();

    for chunk_range in &chunks {
        let chunk_text = get_lines_text(&lines, chunk_range.start_line, chunk_range.end_line);
        if chunk_text.trim().is_empty() {
            continue;
        }

        let (chunk_type, symbol_name, breadcrumb_suffix) = get_chunk_metadata(
            root,
            source.as_bytes(),
            chunk_range.start_line,
            chunk_range.end_line,
        );

        // Build breadcrumb with encoded symbol name
        let breadcrumb = if breadcrumb_suffix.is_empty() {
            base_breadcrumb.clone()
        } else {
            format!(
                "{}:{}",
                base_breadcrumb,
                encode_path_component(&breadcrumb_suffix)
            )
        };

        // Determine chunk_kind based on split type
        let chunk_kind = if chunk_range.from_fallback {
            "fallback-split".to_string()
        } else if chunk_range.from_degraded_ast_split {
            "degraded-ast-split".to_string()
        } else if import_end_line > 0 && chunk_range.end_line <= import_end_line {
            "imports".to_string()
        } else {
            "content".to_string()
        };

        result.push(PartitionedChunk {
            source_uri: file_path.to_string(),
            catalog: catalog.to_string(),
            content_hash: content_hash.to_string(),
            breadcrumb,
            text: chunk_text,
            start_line: chunk_range.start_line,
            end_line: chunk_range.end_line,
            chunk_type,
            chunk_kind,
            symbol_name,
            split_part_ordinal: None,
            split_part_count: None,
        });
    }

    result
}
