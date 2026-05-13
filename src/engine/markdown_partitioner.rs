//! Purpose: Heading-based markdown chunking with breadcrumb generation from heading hierarchy.
//! Edit here when: Changing markdown split boundaries (headings, fenced code blocks, block quotes, paragraphs) or heading-slug behavior.
//! Do not edit here for: TypeScript chunking (see `partitioner/`), breadcrumb-encoding primitives (see `breadcrumb.rs`), strategy dispatch (see `chunker.rs`).

#![allow(dead_code)]

use super::breadcrumb::{encode_path_component, slugify_heading};
use github_slugger::Slugger;

/// Partition a Markdown file into chunks
pub fn partition_markdown(
    source: &str,
    config: &super::partitioner::PartitionConfig,
    file_path: &str,
    catalog: &str,
) -> Result<Vec<super::partitioner::PartitionedChunk>, super::partitioner::PartitionError> {
    use super::partitioner::PartitionedChunk;
    use sha2::{Digest, Sha256};

    let lines: Vec<&str> = source.lines().collect();
    let mut chunks = Vec::new();

    // Compute content hash
    let content_hash = {
        let mut hasher = Sha256::new();
        hasher.update(source.as_bytes());
        format!("sha256:{}", hex::encode(hasher.finalize()))
    };

    // Build breadcrumb prefix with encoded components
    let encoded_package = encode_path_component(&config.package_name);
    let encoded_file_name = encode_path_component(&config.file_name);
    let breadcrumb_prefix = if encoded_package.is_empty() {
        encoded_file_name
    } else {
        format!("{}:{}", encoded_package, encoded_file_name)
    };

    // Create a slugger for heading slugification (shared across the document)
    let mut slugger = Slugger::default();

    // Find all section boundaries (headings)
    let mut section_starts: Vec<usize> = Vec::new();
    let mut in_code_block = false;

    for (i, line) in lines.iter().enumerate() {
        // Track code blocks
        if line.trim().starts_with("```") {
            in_code_block = !in_code_block;
        }

        // Skip headings inside code blocks
        if in_code_block {
            continue;
        }

        // Detect headings
        if line.trim().starts_with('#') {
            section_starts.push(i);
        }
    }

    // Add end boundary
    section_starts.push(lines.len());

    // If no sections, treat entire file as one chunk
    if section_starts.len() == 1 {
        let text = lines.join("\n");
        if !text.trim().is_empty() {
            chunks.push(PartitionedChunk {
                source_uri: file_path.to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.clone(),
                breadcrumb: breadcrumb_prefix.clone(),
                text,
                start_line: 1,
                end_line: lines.len(),
                chunk_type: "markdown".to_string(),
                chunk_kind: "content".to_string(),
                symbol_name: None,
                split_part_ordinal: None,
                split_part_count: None,
            });
        }
        return Ok(chunks);
    }

    // Create chunks for each section
    for i in 0..section_starts.len() - 1 {
        let start_idx = section_starts[i];
        let end_idx = section_starts[i + 1];

        if start_idx >= end_idx {
            continue;
        }

        let section_lines = &lines[start_idx..end_idx];
        let section_text = section_lines.join("\n");

        // Skip empty sections
        if section_text.trim().is_empty() {
            continue;
        }

        // Get heading for breadcrumb - slugify for consistent heading IDs
        let heading = extract_heading_text(section_lines[0]);
        let breadcrumb = if let Some(h) = &heading {
            let slug = slugify_heading(&mut slugger, h);
            format!("{}:{}", breadcrumb_prefix, slug)
        } else {
            breadcrumb_prefix.clone()
        };

        // If section is oversized, split it further
        if section_text.len() > config.target_size {
            split_oversized_section(
                section_lines,
                start_idx + 1, // 1-indexed
                config,
                &breadcrumb,
                file_path,
                catalog,
                &content_hash,
                &mut chunks,
            );
        } else {
            chunks.push(PartitionedChunk {
                source_uri: file_path.to_string(),
                catalog: catalog.to_string(),
                content_hash: content_hash.clone(),
                breadcrumb,
                text: section_text,
                start_line: start_idx + 1, // 1-indexed
                end_line: end_idx,
                chunk_type: "section".to_string(),
                chunk_kind: "content".to_string(),
                symbol_name: heading,
                split_part_ordinal: None,
                split_part_count: None,
            });
        }
    }

    Ok(chunks)
}

/// Extract heading text from a markdown heading line
fn extract_heading_text(line: &str) -> Option<String> {
    let trimmed = line.trim();

    // ATX-style heading (# Heading)
    if trimmed.starts_with('#') {
        let text = trimmed.trim_start_matches('#').trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }

    // Setext-style heading (underlined with === or ---)
    // Would need to look at previous line

    None
}

/// Split an oversized section into smaller chunks
#[allow(clippy::too_many_arguments)]
fn split_oversized_section(
    lines: &[&str],
    start_line: usize,
    config: &super::partitioner::PartitionConfig,
    breadcrumb: &str,
    file_path: &str,
    catalog: &str,
    content_hash: &str,
    chunks: &mut Vec<super::partitioner::PartitionedChunk>,
) {
    use super::partitioner::PartitionedChunk;

    // Find split points within the section
    let mut split_points: Vec<(usize, usize)> = Vec::new();
    let mut current_start = 0;
    let mut current_size = 0;
    let mut in_code_block = false;

    for (i, line) in lines.iter().enumerate() {
        // Track code blocks
        if line.trim().starts_with("```") {
            in_code_block = !in_code_block;
        }

        let line_size = line.len() + 1; // +1 for newline

        // Check if we should split here
        let should_split = current_size + line_size > config.target_size
            && current_size > 0
            && !in_code_block
            && (line.trim().is_empty()
                || line.trim().starts_with("```")
                || line.trim().starts_with('>')
                || line.trim().starts_with('-')
                || line.trim().starts_with('*'));

        if should_split {
            split_points.push((current_start, i));
            current_start = i;
            current_size = 0;
        }

        current_size += line_size;
    }

    // Add final chunk
    if current_start < lines.len() {
        split_points.push((current_start, lines.len()));
    }

    // If we only have one chunk and it's still oversized, split by lines
    if split_points.len() == 1 && lines.join("\n").len() > config.target_size {
        split_by_lines_fallback(
            lines,
            start_line,
            config,
            breadcrumb,
            file_path,
            catalog,
            content_hash,
            chunks,
        );
        return;
    }

    // Emit chunks - breadcrumb is identical for all parts, differ only in split_part_ordinal/count
    let total_parts = split_points.len();
    for (i, (start_idx, end_idx)) in split_points.iter().enumerate() {
        let chunk_lines = &lines[*start_idx..*end_idx];
        let chunk_text = chunk_lines.join("\n");

        if chunk_text.trim().is_empty() {
            continue;
        }

        chunks.push(PartitionedChunk {
            source_uri: file_path.to_string(),
            catalog: catalog.to_string(),
            content_hash: content_hash.to_string(),
            breadcrumb: breadcrumb.to_string(),
            text: chunk_text,
            start_line: start_line + start_idx,
            end_line: start_line + end_idx - 1,
            chunk_type: "section".to_string(),
            chunk_kind: "content".to_string(),
            symbol_name: None,
            split_part_ordinal: Some(i + 1),
            split_part_count: Some(total_parts),
        });
    }
}

/// Fallback: split by lines when other methods fail
#[allow(clippy::too_many_arguments)]
fn split_by_lines_fallback(
    lines: &[&str],
    start_line: usize,
    config: &super::partitioner::PartitionConfig,
    breadcrumb: &str,
    file_path: &str,
    catalog: &str,
    content_hash: &str,
    chunks: &mut Vec<super::partitioner::PartitionedChunk>,
) {
    use super::partitioner::PartitionedChunk;

    // First pass: collect all split boundaries
    let mut split_boundaries: Vec<(usize, usize)> = Vec::new();
    let mut current_start = 0;
    let mut current_size = 0;

    for (i, line) in lines.iter().enumerate() {
        let line_size = line.len() + 1;

        if current_size + line_size > config.target_size && current_size > 0 {
            split_boundaries.push((current_start, i));
            current_start = i;
            current_size = 0;
        }
        current_size += line_size;
    }

    // Add final boundary
    if current_start < lines.len() {
        split_boundaries.push((current_start, lines.len()));
    }

    let total_parts = split_boundaries.len();

    // Second pass: emit chunks with proper part numbers
    for (i, (start_idx, end_idx)) in split_boundaries.iter().enumerate() {
        let chunk_lines = &lines[*start_idx..*end_idx];
        let chunk_text = chunk_lines.join("\n");

        if chunk_text.trim().is_empty() {
            continue;
        }

        chunks.push(PartitionedChunk {
            source_uri: file_path.to_string(),
            catalog: catalog.to_string(),
            content_hash: content_hash.to_string(),
            breadcrumb: breadcrumb.to_string(),
            text: chunk_text,
            start_line: start_line + start_idx,
            end_line: start_line + end_idx - 1,
            chunk_type: "markdown".to_string(),
            chunk_kind: "content".to_string(),
            symbol_name: None,
            split_part_ordinal: Some(i + 1),
            split_part_count: Some(total_parts),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::partition_markdown;
    use crate::engine::partitioner::{PartitionConfig, PartitionedChunk};
    use insta::assert_snapshot;

    fn format_chunks(chunks: &[PartitionedChunk]) -> String {
        let mut result = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            result.push_str(&format!(
                "=== CHUNK {} ===\nBreadcrumb: {}\nType: {}\nLines: {}-{}\nSize: {} chars\nPreview:\n{}\n\n",
                i + 1,
                chunk.breadcrumb,
                chunk.chunk_type,
                chunk.start_line,
                chunk.end_line,
                chunk.text.len(),
                chunk.text.lines().take(6).collect::<Vec<_>>().join("\n")
            ));
        }
        result
    }

    #[test]
    fn test_markdown_simple() {
        let source = r#"# Main Title

This is intro paragraph.

## Section 1

Some text here.

### Subsection

More content.

## Section 2

Final paragraph.
"#;

        let config = PartitionConfig {
            file_name: "test.md".to_string(),
            package_name: "@test/docs".to_string(),
            ..Default::default()
        };

        let chunks = partition_markdown(source, &config, "test.md", "test").unwrap();
        assert_snapshot!(format_chunks(&chunks));
    }

    #[test]
    fn test_markdown_with_code() {
        let source = include_str!("../../test_artifacts/test.md");
        let config = PartitionConfig {
            file_name: "API.md".to_string(),
            package_name: "monodex".to_string(),
            ..Default::default()
        };

        let chunks = partition_markdown(source, &config, "API.md", "test").unwrap();
        assert_snapshot!(format_chunks(&chunks));
    }

    #[test]
    fn test_markdown_heading_slug_duplicates() {
        // Test that duplicate headings get unique slugs using github-slugger
        let source = r#"## API: Configuration

Config docs here.

## Examples

First examples section.

## Examples

Second examples section (duplicate heading).
"#;

        let config = PartitionConfig {
            file_name: "README.md".to_string(),
            package_name: "pkg".to_string(),
            ..Default::default()
        };

        let chunks = partition_markdown(source, &config, "README.md", "test").unwrap();

        // Expected breadcrumbs:
        // - "API: Configuration" -> "api-configuration" (colon removed by slugifier)
        // - "Examples" -> "examples"
        // - "Examples" (second) -> "examples-1"
        assert_eq!(chunks.len(), 3);
        assert!(
            chunks[0].breadcrumb.contains("api-configuration"),
            "First heading should have slug 'api-configuration', got: {}",
            chunks[0].breadcrumb
        );
        assert!(
            chunks[1].breadcrumb.ends_with(":examples"),
            "First 'Examples' should have slug 'examples', got: {}",
            chunks[1].breadcrumb
        );
        assert!(
            chunks[2].breadcrumb.ends_with(":examples-1"),
            "Second 'Examples' should have slug 'examples-1', got: {}",
            chunks[2].breadcrumb
        );
    }

    #[test]
    fn test_markdown_oversized_section_split() {
        // Test that oversized sections share identical breadcrumb but differ in split metadata
        let long_text = "x".repeat(7000); // Exceeds 6000 char target
        let source = format!(
            r#"## Long Section

{}
"#,
            long_text
        );

        let config = PartitionConfig {
            file_name: "test.md".to_string(),
            package_name: "pkg".to_string(),
            ..Default::default()
        };

        let chunks = partition_markdown(&source, &config, "test.md", "test").unwrap();

        // Should be split into multiple chunks
        assert!(
            chunks.len() > 1,
            "Expected multiple chunks for oversized section, got {}",
            chunks.len()
        );

        // All chunks should have identical breadcrumbs
        let first_breadcrumb = &chunks[0].breadcrumb;
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(
                &chunk.breadcrumb, first_breadcrumb,
                "Chunk {} breadcrumb '{}' differs from first chunk '{}'",
                i, chunk.breadcrumb, first_breadcrumb
            );
        }

        // Chunks should differ in split_part_ordinal
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(
                chunk.split_part_ordinal,
                Some(i + 1),
                "Chunk {} should have split_part_ordinal = {}",
                i,
                i + 1
            );
            assert!(
                chunk.split_part_count.is_some(),
                "Chunk {} should have split_part_count",
                i
            );
        }

        // All should have same split_part_count
        let total_count = chunks[0].split_part_count.unwrap();
        assert_eq!(
            total_count,
            chunks.len(),
            "split_part_count should equal total chunks"
        );
        for chunk in &chunks {
            assert_eq!(chunk.split_part_count, Some(total_count));
        }
    }
}
