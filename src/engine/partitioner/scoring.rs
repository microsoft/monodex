//! Purpose: Chunk-quality scoring (0-100%) and `ChunkQualityReport` for `audit-chunks`.
//! Edit here when: Adding or modifying quality metrics, scoring formulas, or reports.
//! Do not edit here for: Debug logging (see `debug.rs`), split logic (see `split_search.rs`), chunk types (see `types.rs`).

use super::types::{PartitionedChunk, SMALL_CHUNK_CHARS, TARGET_CHARS};

/// Compute a 0-100% quality score for a partitioned file.
///
/// The score is a maintainer triage heuristic for `audit-chunks` and `dump-chunks`,
/// not a calibrated metric. It measures two independent dimensions:
///
/// - **Size badness**: penalizes chunks outside the healthy band `[SMALL_CHUNK_CHARS, TARGET_CHARS]`.
///   Chunks within the band have zero penalty. A single whole-file chunk at or below
///   `TARGET_CHARS` is never penalized (it cannot be grown and must not be split).
///
/// - **Count badness**: penalizes producing far more chunks than the content requires.
///   Moderate over-splitting is forgiven; the penalty rises sharply as chunk count
///   approaches the all-runt case.
///
/// The two badnesses combine multiplicatively with no exponents. Scores below roughly
/// 85% are worth inspecting; scores below roughly 60% usually indicate tiny chunks,
/// oversized chunks, or severe over-splitting.
pub fn chunk_quality_score(chunks: &[PartitionedChunk], file_chars: usize) -> f64 {
    if chunks.is_empty() || file_chars == 0 {
        return 100.0;
    }

    let chunk_count = chunks.len();
    let chunk_sizes: Vec<usize> = chunks.iter().map(|c| c.text.len()).collect();
    let total_chars: usize = chunk_sizes.iter().sum();

    // Special case: a single whole-file chunk at or below TARGET_CHARS is never penalized.
    // Such a chunk is the entire file; it cannot be grown and must not be split,
    // so a small whole-file chunk is not a runt.
    if chunk_count == 1 && chunk_sizes[0] <= TARGET_CHARS {
        return 100.0;
    }

    // Compute per-chunk size penalties.
    // 0 for chunks in [SMALL_CHUNK_CHARS, TARGET_CHARS]
    // (SMALL_CHUNK_CHARS - size) / SMALL_CHUNK_CHARS for chunks below SMALL_CHUNK_CHARS
    // ((size - TARGET_CHARS) / TARGET_CHARS).min(1.0) for chunks above TARGET_CHARS
    let size_penalties: Vec<f64> = chunk_sizes
        .iter()
        .map(|&size| {
            if (SMALL_CHUNK_CHARS..=TARGET_CHARS).contains(&size) {
                0.0
            } else if size < SMALL_CHUNK_CHARS {
                (SMALL_CHUNK_CHARS - size) as f64 / SMALL_CHUNK_CHARS as f64
            } else {
                // size > TARGET_CHARS
                ((size - TARGET_CHARS) as f64 / TARGET_CHARS as f64).min(1.0)
            }
        })
        .collect();

    // size_badness is the mean of per-chunk size penalties, in [0, 1] by construction.
    let size_badness = size_penalties.iter().sum::<f64>() / chunk_count.max(1) as f64;

    // Compute count_badness.
    // ideal = max(1, total_chars.div_ceil(TARGET_CHARS))
    // worst = max(ideal + 1, total_chars / SMALL_CHUNK_CHARS)
    // surplus = chunk_count.saturating_sub(ideal)
    // count_badness = (surplus / (worst - ideal)).min(1.0)
    let ideal = total_chars.div_ceil(TARGET_CHARS).max(1);
    let worst = (ideal + 1).max(total_chars / SMALL_CHUNK_CHARS);
    let surplus = chunk_count.saturating_sub(ideal);
    let count_badness = (surplus as f64 / (worst - ideal) as f64).min(1.0);

    // Combine multiplicatively.
    // Both badnesses are in [0, 1], so (1 - badness) is in [0, 1].
    // The product is in [0, 1], and 100 * product is in [0, 100].
    let score = 100.0 * (1.0 - size_badness) * (1.0 - count_badness);

    // Final clamp as a numerical safety net only; every intermediate value
    // is already in range by construction.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a minimal PartitionedChunk with the given text size.
    fn make_chunk(size: usize) -> PartitionedChunk {
        PartitionedChunk {
            source_uri: "test.ts".to_string(),
            catalog: "test".to_string(),
            content_hash: "hash".to_string(),
            breadcrumb: "test.ts".to_string(),
            text: "x".repeat(size),
            start_line: 1,
            end_line: 1,
            chunk_type: "code".to_string(),
            chunk_kind: "content".to_string(),
            symbol_name: None,
            split_part_ordinal: None,
            split_part_count: None,
        }
    }

    #[test]
    fn test_empty_input_scores_100() {
        let chunks: Vec<PartitionedChunk> = vec![];
        let score = chunk_quality_score(&chunks, 0);
        assert_eq!(score, 100.0);
    }

    #[test]
    fn test_all_target_sized_chunks_scores_100() {
        // A partition of all TARGET_CHARS-sized chunks scores 100.
        let chunks = vec![make_chunk(TARGET_CHARS), make_chunk(TARGET_CHARS)];
        let file_chars = 2 * TARGET_CHARS;
        let score = chunk_quality_score(&chunks, file_chars);
        assert_eq!(score, 100.0);
    }

    #[test]
    fn test_oversized_single_chunk_scores_0() {
        // An oversized single chunk at twice TARGET_CHARS scores 0.
        let chunks = vec![make_chunk(2 * TARGET_CHARS)];
        let file_chars = 2 * TARGET_CHARS;
        let score = chunk_quality_score(&chunks, file_chars);
        // size penalty = ((2*TARGET - TARGET) / TARGET).min(1.0) = 1.0
        // size_badness = 1.0
        // (1 - size_badness) = 0, so score = 0
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_many_runt_chunks_scores_near_0() {
        // A file split into many sub-SMALL_CHUNK_CHARS runts scores near 0.
        let runt_size = 100; // well below SMALL_CHUNK_CHARS (500)
        let num_runts = 20;
        let chunks: Vec<PartitionedChunk> = (0..num_runts).map(|_| make_chunk(runt_size)).collect();
        let file_chars = runt_size * num_runts;
        let score = chunk_quality_score(&chunks, file_chars);
        // Each runt has size penalty = (500 - 100) / 500 = 0.8
        // size_badness = 0.8
        // ideal = max(1, 2000 / 6000) = 1
        // worst = max(2, 2000 / 500) = max(2, 4) = 4
        // surplus = 20 - 1 = 19
        // count_badness = min(1.0, 19 / 3) = 1.0
        // score = 100 * (1 - 0.8) * (1 - 1.0) = 100 * 0.2 * 0 = 0
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_single_small_whole_file_chunk_scores_100() {
        // A single whole-file chunk below TARGET_CHARS scores 100.
        let small_size = 1000; // below TARGET_CHARS (6000)
        let chunks = vec![make_chunk(small_size)];
        let file_chars = small_size;
        let score = chunk_quality_score(&chunks, file_chars);
        assert_eq!(score, 100.0);
    }

    #[test]
    fn test_size_healthy_but_over_split_scores_high_but_below_100() {
        // A size-healthy file split into roughly twice the ideal chunk count
        // scores high but below 100 (count-penalized only).
        // Use chunks in the healthy band [500, 6000].
        let chunk_size = 3000; // in healthy band
        let num_chunks = 4;
        let chunks: Vec<PartitionedChunk> =
            (0..num_chunks).map(|_| make_chunk(chunk_size)).collect();
        let file_chars = chunk_size * num_chunks; // 12000 chars

        // ideal = max(1, 12000 / 6000) = 2
        // worst = max(3, 12000 / 500) = max(3, 24) = 24
        // surplus = 4 - 2 = 2
        // count_badness = 2 / 22 ≈ 0.091
        // size_badness = 0 (all chunks in healthy band)
        // score = 100 * 1.0 * (1 - 0.091) ≈ 90.9
        let score = chunk_quality_score(&chunks, file_chars);
        assert!(score > 85.0 && score < 100.0, "score was {}", score);
    }

    #[test]
    fn test_chunk_below_small_chunk_chars_has_penalty() {
        // A single chunk below SMALL_CHUNK_CHARS (but not a whole file) should have
        // a non-zero size penalty. But since it's the only chunk and <= TARGET_CHARS,
        // it gets the special case and scores 100.
        // So test with two chunks: one healthy, one small.
        let chunks = vec![make_chunk(TARGET_CHARS), make_chunk(100)]; // 100 < SMALL_CHUNK_CHARS
        let file_chars = TARGET_CHARS + 100;
        let score = chunk_quality_score(&chunks, file_chars);
        // First chunk: size penalty = 0 (in healthy band)
        // Second chunk: size penalty = (500 - 100) / 500 = 0.8
        // size_badness = (0 + 0.8) / 2 = 0.4
        // ideal = max(1, 6100 / 6000) = 2
        // worst = max(3, 6100 / 500) = max(3, 13) = 13
        // surplus = 2 - 2 = 0
        // count_badness = 0
        // score = 100 * (1 - 0.4) * (1 - 0) = 60
        assert!((score - 60.0).abs() < 0.1, "score was {}", score);
    }

    #[test]
    fn test_chunk_above_target_chars_has_penalty() {
        // A chunk above TARGET_CHARS should have a size penalty.
        let oversized = TARGET_CHARS + 1000; // 7000
        let chunks = vec![make_chunk(oversized)];
        let file_chars = oversized;
        let score = chunk_quality_score(&chunks, file_chars);
        // Single chunk but > TARGET_CHARS, so no special case.
        // size penalty = ((7000 - 6000) / 6000).min(1.0) = 1000/6000 ≈ 0.167
        // size_badness = 0.167
        // ideal = max(1, 7000 / 6000) = 2
        // worst = max(3, 7000 / 500) = max(3, 14) = 14
        // surplus = 1 - 2 = 0 (saturating_sub)
        // count_badness = 0
        // score = 100 * (1 - 0.167) * 1 = 83.3
        assert!(score > 80.0 && score < 90.0, "score was {}", score);
    }
}
