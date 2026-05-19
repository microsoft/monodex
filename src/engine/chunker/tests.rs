//! Purpose: Test suite for chunker strategy dispatch.
//! Edit here when: Adding or modifying tests for chunker strategy dispatch.
//! Do not edit here for: Chunker implementation (see `../chunker.rs`).

use super::*;
use crate::engine::crawl_config::load_compiled_crawl_config;

/// Helper to create a test chunk context
fn test_context(blob_id: &str, relative_path: &str, package_name: &str) -> ChunkContext {
    ChunkContext {
        catalog: "test-catalog".to_string(),
        label_id: "test-catalog:main".to_string(),
        package_name: package_name.to_string(),
        relative_path: relative_path.to_string(),
        blob_id: blob_id.to_string(),
        source_uri: format!("/repo/{}", relative_path),
    }
}

/// Helper to get default strategy for a path (for tests that want default behavior)
fn default_strategy(path: &str) -> ChunkingStrategy {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let paths = crate::paths::Paths::for_test(temp_dir.path().into());
    let config = load_compiled_crawl_config(&paths, None).expect("Embedded config should be valid");
    config.get_strategy(path)
}

/// Test that same content + path produces same file_id
#[test]
fn test_same_content_path_produces_same_file_id() {
    let content = r#"
export function hello() {
    console.log("Hello, world!");
}
"#;
    let ctx = test_context("abc123", "src/index.ts", "@test/pkg");
    let strategy = default_strategy(&ctx.relative_path);

    let chunks1 = chunk_content(content, &ctx, 6000, strategy).unwrap();
    let chunks2 = chunk_content(content, &ctx, 6000, strategy).unwrap();

    assert_eq!(chunks1.len(), chunks2.len());
    for (c1, c2) in chunks1.iter().zip(chunks2.iter()) {
        assert_eq!(
            c1.file_id, c2.file_id,
            "Same content+path should produce same file_id"
        );
        assert_eq!(
            c1.row_id(),
            c2.row_id(),
            "Same content+path should produce same row_id"
        );
    }
}

/// Test that path changes produce different file_id (expected behavior)
/// Path is part of semantic identity because it affects breadcrumb context
#[test]
fn test_path_change_produces_different_file_id() {
    let content = r#"
export function hello() {
    console.log("Hello, world!");
}
"#;
    let ctx1 = test_context("abc123", "src/index.ts", "@test/pkg");
    let ctx2 = test_context("abc123", "lib/index.ts", "@test/pkg");

    let chunks1 =
        chunk_content(content, &ctx1, 6000, default_strategy(&ctx1.relative_path)).unwrap();
    let chunks2 =
        chunk_content(content, &ctx2, 6000, default_strategy(&ctx2.relative_path)).unwrap();

    assert!(!chunks1.is_empty() && !chunks2.is_empty());
    assert_ne!(
        chunks1[0].file_id, chunks2[0].file_id,
        "Different paths should produce different file_id"
    );
    assert_ne!(
        chunks1[0].row_id(),
        chunks2[0].row_id(),
        "Different paths should produce different row_id"
    );
}

/// Test that same content at different paths = different chunks (semantic context matters)
/// This verifies that path renames create new chunks even if content is identical
#[test]
fn test_content_at_different_paths_creates_different_chunks() {
    let content = r#"
export class JsonFile {
    public static load(path: string): object {
        return JSON.parse(fs.readFileSync(path, 'utf-8'));
    }
}
"#;
    // Simulate a file moving from libraries/package1 to libraries/package2
    let ctx1 = test_context(
        "abc123",
        "libraries/package1/src/JsonFile.ts",
        "@scope/package1",
    );
    let ctx2 = test_context(
        "abc123",
        "libraries/package2/src/JsonFile.ts",
        "@scope/package2",
    );

    let chunks1 =
        chunk_content(content, &ctx1, 6000, default_strategy(&ctx1.relative_path)).unwrap();
    let chunks2 =
        chunk_content(content, &ctx2, 6000, default_strategy(&ctx2.relative_path)).unwrap();

    // Both should produce chunks
    assert!(!chunks1.is_empty() && !chunks2.is_empty());

    // File IDs should be different (path is part of identity)
    assert_ne!(chunks1[0].file_id, chunks2[0].file_id);

    // Row IDs should be different
    assert_ne!(chunks1[0].row_id(), chunks2[0].row_id());

    // Breadcrumbs should reflect the different package context (percent-encoded @scope)
    assert!(
        chunks1[0].breadcrumb.starts_with("%40scope/package1"),
        "Breadcrumb should start with %40scope/package1, got: {}",
        chunks1[0].breadcrumb
    );
    assert!(
        chunks2[0].breadcrumb.starts_with("%40scope/package2"),
        "Breadcrumb should start with %40scope/package2, got: {}",
        chunks2[0].breadcrumb
    );
}

/// Test that blob_id changes produce different file_id
#[test]
fn test_content_change_produces_different_file_id() {
    let content = r#"
export function hello() {
    console.log("Hello, world!");
}
"#;
    // Same path, different blob_id (different content)
    let ctx1 = test_context("abc123", "src/index.ts", "@test/pkg");
    let ctx2 = test_context("def456", "src/index.ts", "@test/pkg");

    let chunks1 =
        chunk_content(content, &ctx1, 6000, default_strategy(&ctx1.relative_path)).unwrap();
    let chunks2 =
        chunk_content(content, &ctx2, 6000, default_strategy(&ctx2.relative_path)).unwrap();

    assert!(!chunks1.is_empty() && !chunks2.is_empty());
    assert_ne!(
        chunks1[0].file_id, chunks2[0].file_id,
        "Different blob_id should produce different file_id"
    );
}

/// Test chunk ordinals are assigned correctly
#[test]
fn test_chunk_ordinals_assigned_correctly() {
    // Create a file large enough to be split into multiple chunks
    let mut content = String::new();
    for i in 0..50 {
        content.push_str(&format!(
            r#"
export function function_{}() {{
    console.log("Function {}");
    // This is a long comment to increase the size of this function
    // Adding more lines to make it larger
    // And even more lines to ensure it exceeds the target size
    let x = {};
    let y = {};
    let z = x + y;
    return z;
}}
"#,
            i,
            i,
            i * 10,
            i * 20
        ));
    }

    let ctx = test_context("abc123", "src/large.ts", "@test/pkg");
    let chunks = chunk_content(&content, &ctx, 1000, default_strategy(&ctx.relative_path)).unwrap(); // Small target to force splits

    // Should have multiple chunks
    assert!(
        chunks.len() > 1,
        "Expected multiple chunks, got {}",
        chunks.len()
    );

    // Check ordinals are sequential starting from 1
    for (i, chunk) in chunks.iter().enumerate() {
        assert_eq!(
            chunk.chunk_ordinal,
            i + 1,
            "Chunk ordinal should be {}",
            i + 1
        );
    }

    // All chunks should have the same chunk_count
    let expected_count = chunks.len();
    for chunk in &chunks {
        assert_eq!(chunk.chunk_count, expected_count);
    }

    // Chunks should have non-empty file_id
    for chunk in &chunks {
        assert!(!chunk.file_id.is_empty());
        assert_eq!(chunk.file_id.len(), 16, "file_id should be 16 hex chars");
    }
}

/// B.1 Regression test: Strategy override changes chunking behavior
///
/// This test proves that passing a strategy override from discovered crawl config
/// actually changes how content is chunked. We use markdown vs lineBased as the
/// test case because they produce measurably different chunk boundaries.
///
/// - Markdown strategy splits at heading boundaries
/// - lineBased strategy splits at line count boundaries (no heading awareness)
#[test]
fn test_strategy_override_changes_chunking_behavior() {
    // Markdown content with clear heading structure
    let content = r#"# Main Title

This is the introduction paragraph.

## Section One

Content for section one.

### Subsection A

More content here.

## Section Two

Content for section two.
"#;

    let ctx = test_context("abc123", "docs/README.md", "@test/pkg");

    // Chunk with markdown strategy (default for .md files)
    let markdown_chunks = chunk_content(content, &ctx, 6000, ChunkingStrategy::Markdown).unwrap();

    // Chunk with lineBased strategy (override from crawl config)
    let linebased_chunks = chunk_content(content, &ctx, 6000, ChunkingStrategy::LineBased).unwrap();

    // Both should produce chunks
    assert!(
        !markdown_chunks.is_empty(),
        "Markdown strategy should produce chunks"
    );
    assert!(
        !linebased_chunks.is_empty(),
        "lineBased strategy should produce chunks"
    );

    // The key assertion: different strategies produce different chunking
    // Markdown strategy splits at headings, producing chunks with heading breadcrumbs
    // lineBased strategy splits at line boundaries, producing chunks without heading awareness

    // Check that markdown chunks have heading-based breadcrumbs (slugified)
    // (e.g., "main-title", "section-one", etc.)
    let has_heading_breadcrumbs = markdown_chunks.iter().any(|c| {
        c.breadcrumb.contains("main-title")
            || c.breadcrumb.contains("section-one")
            || c.breadcrumb.contains("section-two")
    });
    assert!(
        has_heading_breadcrumbs,
        "Markdown chunks should have heading-based breadcrumbs. Got: {:?}",
        markdown_chunks
            .iter()
            .map(|c| &c.breadcrumb)
            .collect::<Vec<_>>()
    );

    // Check that the number of chunks differs OR the chunk boundaries differ
    // This is a stronger assertion that strategies actually behave differently
    if markdown_chunks.len() == linebased_chunks.len() {
        // Same count, but boundaries should differ
        let markdown_lines: Vec<_> = markdown_chunks
            .iter()
            .map(|c| (c.start_line, c.end_line))
            .collect();
        let linebased_lines: Vec<_> = linebased_chunks
            .iter()
            .map(|c| (c.start_line, c.end_line))
            .collect();
        assert_ne!(
            markdown_lines, linebased_lines,
            "Different strategies should produce different chunk boundaries"
        );
    }
    // If counts differ, that's already proof of different behavior

    // Additional check: markdown chunks should have chunk_type reflecting heading structure
    // (this verifies the partitioner actually ran the markdown-specific logic)
    let has_section_chunks = markdown_chunks
        .iter()
        .any(|c| c.chunk_type.contains("heading") || c.chunk_type.contains("section"));
    assert!(
        has_section_chunks || markdown_chunks.len() > 1,
        "Markdown strategy should recognize heading structure (either via chunk_type or multiple chunks)"
    );
}

/// B.1 Regression test: Strategy override to "typescript" for .md file
///
/// An extreme test: treating markdown as TypeScript should still produce chunks
/// (via fallback), but with different characteristics than markdown chunking.
#[test]
fn test_strategy_override_typescript_for_markdown() {
    let content = r#"# Title

Some text here.
"#;

    let ctx = test_context("abc123", "docs/README.md", "@test/pkg");

    // Chunk with typescript strategy (override from crawl config)
    let ts_chunks = chunk_content(content, &ctx, 6000, ChunkingStrategy::TypeScript).unwrap();

    // Should still produce chunks (fallback path since markdown isn't valid TS)
    assert!(
        !ts_chunks.is_empty(),
        "TypeScript strategy should produce chunks even for non-TS content"
    );

    // File ID should still be computed correctly
    assert_eq!(
        ts_chunks[0].file_id.len(),
        16,
        "file_id should be 16 hex chars"
    );
}
