//! Purpose: Chunk-quality scoring (0-100%) and `ChunkQualityReport` for `audit-chunks`.
//! Edit here when: Adding or modifying quality metrics, scoring formulas, or reports.
//! Do not edit here for: Debug logging (see `debug.rs`), split logic (see `split_search.rs`), chunk types (see `types.rs`).

use super::types::{PartitionedChunk, SMALL_CHUNK_CHARS, TARGET_CHARS};

pub fn chunk_quality_score(chunks: &[PartitionedChunk], file_chars: usize) -> f64 {
    if chunks.is_empty() || file_chars == 0 {
        return 100.0;
    }

    let max_chunk_size = TARGET_CHARS.min(file_chars);
    let chunk_count = chunks.len();

    // Compute chunk sizes in characters
    let chunk_sizes: Vec<usize> = chunks.iter().map(|c| c.text.len()).collect();

    let total_chars: usize = chunk_sizes.iter().sum();

    // Ideal number of chunks
    let ideal_chunk_count = total_chars.div_ceil(max_chunk_size); // ceil division

    // 1) Count badness: 0 at ideal chunk count, 1 at all 1-char chunks
    let count_badness = if total_chars == ideal_chunk_count {
        0.0
    } else {
        (chunk_count as f64 - ideal_chunk_count as f64)
            / (total_chars as f64 - ideal_chunk_count as f64)
    };

    // Helper: chunk badness (0 at max size, 1 at 1 char)
    // For oversized chunks, weight by how much work is unfinished
    let chunk_badness = |size: usize| -> f64 {
        if size >= max_chunk_size {
            // Estimate: if we could split correctly, we'd get N chunks
            // Weight the badness as if there were N unsplittable chunks
            (size as f64 / max_chunk_size as f64).max(1.0)
        } else {
            ((max_chunk_size - size) as f64 / (max_chunk_size - 1) as f64).powi(2)
        }
    };

    // 2) Micro-chunk badness relative to ideal partition
    let ideal_last_chunk_size =
        total_chars - max_chunk_size * (ideal_chunk_count.saturating_sub(1));
    let ideal_partition_badness = if ideal_chunk_count == 0 {
        0.0
    } else if ideal_chunk_count == 1 {
        chunk_badness(ideal_last_chunk_size)
    } else {
        // All but last chunk are at max size (badness 0), last chunk may be smaller
        chunk_badness(ideal_last_chunk_size)
    };

    let actual_partition_badness: f64 = chunk_sizes.iter().map(|&s| chunk_badness(s)).sum();

    // Normalize by number of chunks, not total chars
    // This gives an average badness per chunk, which is more meaningful
    // Worst case: each chunk has badness 1.0 (either tiny or oversized with ratio 1.0)
    let avg_badness = actual_partition_badness / chunk_count.max(1) as f64;

    // Also compute worst case normalized similarly
    let ideal_avg_badness = ideal_partition_badness / ideal_chunk_count.max(1) as f64;
    let worst_avg_badness = 1.0; // a chunk with badness 1.0 is the worst reasonable case

    let micro_badness = if worst_avg_badness == ideal_avg_badness {
        0.0
    } else {
        (avg_badness - ideal_avg_badness) / (worst_avg_badness - ideal_avg_badness)
    };

    // Clamp for numerical safety
    let count_badness = count_badness.clamp(0.0, 1.0);
    let micro_badness = micro_badness.clamp(0.0, 1.0);

    // Final score: weight micro_badness (beta=1 gives linear penalty)
    let alpha = 1.0;
    let beta = 1.0;
    let score = 100.0 * (1.0 - count_badness).powf(alpha) * (1.0 - micro_badness).powf(beta);

    score.clamp(0.0, 100.0)
}

/// Quality report for chunking results
pub struct ChunkQualityReport {
    /// Quality score (0-100%, higher is better)
    pub score: f64,
    /// Total number of chunks
    pub total_chunks: usize,
    /// Number of small chunks under SMALL_CHUNK_CHARS (likely problematic)
    pub small_chunks: usize,
    /// Smallest chunk in characters
    pub min_chars: usize,
    /// Largest chunk in characters
    pub max_chars: usize,
    /// Mean chunk size in characters
    pub mean_chars: f64,
}

impl ChunkQualityReport {
    pub fn from_chunks(chunks: &[PartitionedChunk], file_chars: usize) -> Self {
        if chunks.is_empty() {
            return Self {
                score: 100.0,
                total_chunks: 0,
                small_chunks: 0,
                min_chars: 0,
                max_chars: 0,
                mean_chars: 0.0,
            };
        }

        let char_counts: Vec<usize> = chunks.iter().map(|c| c.text.len()).collect();

        Self {
            score: chunk_quality_score(chunks, file_chars),
            total_chunks: chunks.len(),
            small_chunks: char_counts
                .iter()
                .filter(|&&c| c < SMALL_CHUNK_CHARS)
                .count(),
            min_chars: *char_counts.iter().min().unwrap(),
            max_chars: *char_counts.iter().max().unwrap(),
            mean_chars: char_counts.iter().sum::<usize>() as f64 / char_counts.len() as f64,
        }
    }

    pub fn format(&self) -> String {
        format!(
            "Score: {:.1}% | Chunks: {} | Small (<{} chars): {} | Chars: {}-{} (mean {:.0})",
            self.score,
            self.total_chunks,
            SMALL_CHUNK_CHARS,
            self.small_chunks,
            self.min_chars,
            self.max_chars,
            self.mean_chars
        )
    }
}
