//! Purpose: Split files into semantically meaningful chunks for embedding.
//! Edit here when: Adding new chunking strategies, modifying chunk structure, or changing how files are partitioned.
//! Do not edit here for: Changing which files to crawl (use crawl_config.rs), embedding model changes (use parallel_embedder.rs).

use super::crawl_config::ChunkingStrategy;
use super::identity::{CHUNKER_ID, EMBEDDER_ID, compute_file_id, compute_hash, compute_row_id};
use super::markdown_partitioner::partition_markdown;
use super::partitioner::{PartitionConfig, PartitionedChunk, partition_typescript};
use anyhow::Result;

/// Represents a chunk of code or documentation
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The text content of the chunk
    pub text: String,

    /// Source URI (full file path)
    pub source_uri: String,

    /// Catalog name
    pub catalog: String,

    /// Content hash (SHA256) for incremental sync
    pub content_hash: String,

    /// Starting line number (1-indexed)
    pub start_line: usize,

    /// Ending line number (inclusive)
    pub end_line: usize,

    /// Optional symbol name (for functions, classes, etc.)
    pub symbol_name: Option<String>,

    /// Chunk type (e.g., "function", "class", "markdown-section")
    pub chunk_type: String,

    /// Chunk kind (content, imports, changelog, config)
    pub chunk_kind: String,

    /// Breadcrumb path (e.g., "@rushstack/node-core-library:JsonFile.ts:JsonFile.load")
    pub breadcrumb: String,

    /// All labels this chunk belongs to (authoritative)
    pub active_label_ids: Vec<String>,

    /// Implementation identifier for the embedder
    pub embedder_id: String,

    /// Implementation identifier for the chunker
    pub chunker_id: String,

    /// Git blob SHA (content provenance)
    pub blob_id: String,

    /// Package name for breadcrumb (e.g., "@rushstack/node-core-library")
    pub package_name: String,

    /// File ID - semantic file identity (16-char hex string)
    pub file_id: String,

    /// Relative path from catalog base (e.g., "libraries/rush-lib/src/JsonFile.ts")
    pub relative_path: String,

    /// Chunk ordinal within file (1-indexed, ordered by start_line)
    pub chunk_ordinal: usize,

    /// Total number of chunks in this file
    pub chunk_count: usize,

    // --- Split metadata ---
    /// For split sections: which part this is (1-indexed)
    pub split_part_ordinal: Option<usize>,

    /// For split sections: total number of parts
    pub split_part_count: Option<usize>,
}

impl Chunk {
    /// Compute the row ID for this chunk
    pub fn row_id(&self) -> String {
        compute_row_id(&self.file_id, self.chunk_ordinal)
    }
}

/// Context needed for chunking.
pub struct ChunkContext {
    /// Catalog name
    pub catalog: String,
    /// Label ID (internal storage form: catalog:label)
    pub label_id: String,
    /// Package name for breadcrumb
    pub package_name: String,
    /// Relative path from catalog base
    pub relative_path: String,
    /// Git blob SHA
    pub blob_id: String,
    /// Source URI (full path for display)
    pub source_uri: String,
}

/// Chunks file content based on its type
///
/// # Arguments
///
/// * `content` - File content as string
/// * `ctx` - Chunk context with identity information
/// * `target_size` - Target chunk size in characters (default 6000)
/// * `strategy` - Chunking strategy to use
///
/// # Returns
///
/// Vector of chunks or an error
pub fn chunk_content(
    content: &str,
    ctx: &ChunkContext,
    target_size: usize,
    strategy: ChunkingStrategy,
) -> Result<Vec<Chunk>> {
    // Compute file ID from the new identity components
    let file_id = compute_file_id(
        EMBEDDER_ID,
        CHUNKER_ID,
        &ctx.catalog,
        &ctx.blob_id,
        &ctx.relative_path,
    );

    match strategy {
        ChunkingStrategy::TypeScript => {
            chunk_with_partitioner(content, ctx, &file_id, target_size, partition_typescript)
        }
        ChunkingStrategy::Markdown => {
            chunk_with_partitioner(content, ctx, &file_id, target_size, partition_markdown)
        }
        ChunkingStrategy::LineBased => chunk_by_lines(content, &file_id, ctx, target_size, "text"),
        ChunkingStrategy::Skip => Ok(Vec::new()),
    }
}

/// Helper to chunk content using a partitioner function (TypeScript or Markdown).
///
/// Both partitioners have the same signature, so we extract the common scaffold:
/// file_name extraction, PartitionConfig construction, calling the partitioner,
/// mapping to Chunk, sorting by start_line, and assigning ordinals.
fn chunk_with_partitioner<F>(
    content: &str,
    ctx: &ChunkContext,
    file_id: &str,
    target_size: usize,
    partition: F,
) -> Result<Vec<Chunk>, anyhow::Error>
where
    F: FnOnce(
        &str,
        &PartitionConfig,
        &str,
        &str,
    ) -> Result<Vec<PartitionedChunk>, crate::engine::partitioner::PartitionError>,
{
    let file_name = std::path::Path::new(&ctx.relative_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ctx.relative_path.to_string());

    let config = PartitionConfig {
        target_size,
        file_name,
        package_name: ctx.package_name.clone(),
        ..Default::default()
    };

    let partitioned = partition(content, &config, &ctx.source_uri, &ctx.catalog)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut chunks: Vec<Chunk> = partitioned
        .into_iter()
        .enumerate()
        .map(|(i, p)| Chunk::from_partitioned(p, file_id, ctx, i + 1, 0))
        .collect();

    // Assign chunk ordinals (1-indexed, sorted by start_line)
    chunks.sort_by_key(|c| c.start_line);
    let chunk_count = chunks.len();
    for (i, chunk) in chunks.iter_mut().enumerate() {
        chunk.chunk_ordinal = i + 1;
        chunk.chunk_count = chunk_count;
    }

    Ok(chunks)
}

impl Chunk {
    /// Create a chunk from a PartitionedChunk
    fn from_partitioned(
        p: PartitionedChunk,
        file_id: &str,
        ctx: &ChunkContext,
        chunk_ordinal: usize,
        chunk_count: usize,
    ) -> Self {
        Chunk {
            text: p.text,
            source_uri: ctx.source_uri.clone(),
            catalog: ctx.catalog.clone(),
            content_hash: p.content_hash,
            start_line: p.start_line,
            end_line: p.end_line,
            symbol_name: p.symbol_name,
            chunk_type: p.chunk_type,
            chunk_kind: p.chunk_kind,
            breadcrumb: p.breadcrumb,
            active_label_ids: vec![ctx.label_id.clone()],
            embedder_id: EMBEDDER_ID.to_string(),
            chunker_id: CHUNKER_ID.to_string(),
            blob_id: ctx.blob_id.clone(),
            package_name: ctx.package_name.clone(),
            file_id: file_id.to_string(),
            relative_path: ctx.relative_path.clone(),
            chunk_ordinal,
            chunk_count,
            split_part_ordinal: p.split_part_ordinal,
            split_part_count: p.split_part_count,
        }
    }
}

/// Chunk by lines for simple text files
fn chunk_by_lines(
    content: &str,
    file_id: &str,
    ctx: &ChunkContext,
    max_chars: usize,
    chunk_type: &str,
) -> Result<Vec<Chunk>> {
    use super::breadcrumb::encode_path_component;

    let content_hash = compute_hash(content);
    let lines: Vec<&str> = content.lines().collect();

    let mut chunks = Vec::new();
    let mut start = 0;
    let file_name = std::path::Path::new(&ctx.relative_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ctx.relative_path.to_string());

    // Encode file name for breadcrumb
    let encoded_file_name = encode_path_component(&file_name);

    while start < lines.len() {
        let mut end = start;
        let mut size = 0;

        // Build chunk up to max_chars
        while end < lines.len() && size + lines[end].len() < max_chars {
            size += lines[end].len() + 1;
            end += 1;
        }

        // Ensure at least one line per chunk
        if end == start && start < lines.len() {
            end = start + 1;
        }

        let chunk_text = lines[start..end].join("\n");

        // Skip empty or whitespace-only chunks
        if !chunk_text.trim().is_empty() {
            chunks.push(Chunk {
                text: chunk_text,
                source_uri: ctx.source_uri.clone(),
                catalog: ctx.catalog.clone(),
                content_hash: content_hash.clone(),
                start_line: start + 1,
                end_line: end,
                symbol_name: None,
                chunk_type: chunk_type.to_string(),
                chunk_kind: "content".to_string(),
                breadcrumb: encoded_file_name.clone(),
                active_label_ids: vec![ctx.label_id.clone()],
                embedder_id: EMBEDDER_ID.to_string(),
                chunker_id: CHUNKER_ID.to_string(),
                blob_id: ctx.blob_id.clone(),
                package_name: ctx.package_name.clone(),
                file_id: file_id.to_string(),
                relative_path: ctx.relative_path.clone(),
                chunk_ordinal: 0, // Will update after loop
                chunk_count: 0,   // Will update after loop
                split_part_ordinal: None,
                split_part_count: None,
            });
        }

        start = end;
    }

    // Update chunk_ordinal and chunk_count for all chunks
    let total_chunks = chunks.len().max(1);
    for (i, chunk) in chunks.iter_mut().enumerate() {
        chunk.chunk_ordinal = i + 1;
        chunk.chunk_count = total_chunks;
    }

    Ok(chunks)
}

#[cfg(test)]
mod tests;
