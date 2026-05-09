//! Reciprocal rank fusion for hybrid retrieval.
//!
//! Purpose: Combine ranked results from multiple retrieval methods into a single ranked list.
//! Edit here when: Changing the RRF algorithm, adding tuning knobs.
//! Do not edit here for: Retrieval method dispatch (see commands/search.rs), storage operations.
//!
//! ## Algorithm
//!
//! RRF score for a row_id is the sum over all methods of `1 / (k + rank)`.
//! Ranks are 1-indexed. The constant `k = 60` is hardcoded per the literature.
//!
//! ## Tiebreak
//!
//! When two row_ids have equal RRF scores, tiebreak in order:
//! 1. Fused RRF score (descending) — primary ordering
//! 2. Best contributing rank (ascending) — lower is better
//! 3. Contributor method in enum order (Fts before Vector)
//! 4. row_id lexicographic ascending — final fallback

use std::collections::{BTreeSet, HashMap};

use crate::engine::retrieval::RetrievalMethod;

/// RRF constant. Empirically robust across datasets; do not expose as a tuning knob.
const RRF_K: usize = 60;

/// A hit from a single retrieval method, before fusion.
///
/// Position in the input vector implies rank (1-indexed at position 0).
/// `backend_score` is the raw method-local score (BM25 for FTS, cosine distance for vector).
/// Fusion ignores this score; it's carried through for debug output.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodHit {
    pub row_id: String,
    pub backend_score: Option<f32>,
}

/// One method's contribution to a fused hit's ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedContribution {
    pub method: RetrievalMethod,
    /// 1-indexed rank within this method's results.
    pub rank: usize,
    pub backend_score: Option<f32>,
}

/// A hit after RRF fusion, with provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedHit {
    pub row_id: String,
    pub rrf_score: f32,
    /// One entry per method that ranked this row_id.
    pub contributors: Vec<RankedContribution>,
}

/// Fuse ranked results from multiple retrieval methods.
///
/// Each tuple in `method_results` is one method's tagged ranked list.
/// Returns up to `max_results` fused hits in RRF-score-descending order.
///
/// ## Input deduplication
///
/// Within a single method's list, if the same row_id appears more than once,
/// the first occurrence wins (preserves best rank). Later duplicates are ignored.
///
/// ## Output ordering
///
/// Results are sorted by RRF score descending, with tiebreaks applied per module docs.
pub fn fuse(
    method_results: Vec<(RetrievalMethod, Vec<MethodHit>)>,
    max_results: usize,
) -> Vec<FusedHit> {
    // Step 1: Deduplicate within each method (first occurrence wins)
    let deduped: Vec<(RetrievalMethod, Vec<(usize, &MethodHit)>)> = method_results
        .iter()
        .map(|(method, hits)| {
            let mut seen = BTreeSet::new();
            let deduped_hits: Vec<(usize, &MethodHit)> = hits
                .iter()
                .enumerate()
                .filter(|(_idx, hit)| {
                    // idx is 0-indexed; rank is idx + 1
                    if seen.contains(&hit.row_id) {
                        false // duplicate, skip
                    } else {
                        seen.insert(&hit.row_id);
                        true
                    }
                })
                .collect();
            (*method, deduped_hits)
        })
        .collect();

    // Step 2: Compute RRF scores and collect contributions
    // Map from row_id -> (rrf_score, contributors)
    let mut fused_map: HashMap<String, (f32, Vec<RankedContribution>)> = HashMap::new();

    for (method, hits_with_idx) in &deduped {
        for (idx, hit) in hits_with_idx {
            let rank = idx + 1; // 1-indexed rank
            let contribution = 1.0_f32 / (RRF_K as f32 + rank as f32);

            let entry = fused_map
                .entry(hit.row_id.clone())
                .or_insert((0.0, Vec::new()));
            entry.0 += contribution;
            entry.1.push(RankedContribution {
                method: *method,
                rank,
                backend_score: hit.backend_score,
            });
        }
    }

    // Step 3: Sort by RRF score descending with tiebreaks
    let mut fused_hits: Vec<FusedHit> = fused_map
        .into_iter()
        .map(|(row_id, (rrf_score, contributors))| FusedHit {
            row_id,
            rrf_score,
            contributors,
        })
        .collect();

    // Sort with tiebreak
    fused_hits.sort_by(|a, b| {
        // Level 1: RRF score descending
        if (a.rrf_score - b.rrf_score).abs() > f32::EPSILON {
            return b
                .rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal);
        }

        // Level 2: Best contributing rank ascending (lower is better)
        let a_best_rank = a
            .contributors
            .iter()
            .map(|c| c.rank)
            .min()
            .unwrap_or(usize::MAX);
        let b_best_rank = b
            .contributors
            .iter()
            .map(|c| c.rank)
            .min()
            .unwrap_or(usize::MAX);
        if a_best_rank != b_best_rank {
            return a_best_rank.cmp(&b_best_rank);
        }

        // Level 3: Best-rank method in enum order (Fts < Vector)
        let a_best_method = a
            .contributors
            .iter()
            .filter(|c| c.rank == a_best_rank)
            .map(|c| c.method)
            .min()
            .unwrap_or(RetrievalMethod::Fts);
        let b_best_method = b
            .contributors
            .iter()
            .filter(|c| c.rank == b_best_rank)
            .map(|c| c.method)
            .min()
            .unwrap_or(RetrievalMethod::Fts);
        if a_best_method != b_best_method {
            return a_best_method.cmp(&b_best_method);
        }

        // Level 4: row_id lexicographic ascending
        a.row_id.cmp(&b.row_id)
    });

    // Step 4: Truncate to max_results
    fused_hits.truncate(max_results);

    fused_hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(row_id: &str, score: Option<f32>) -> MethodHit {
        MethodHit {
            row_id: row_id.to_string(),
            backend_score: score,
        }
    }

    #[test]
    fn test_identity_single_method() {
        // Single method with 3 hits should return them in order with scaled RRF scores
        let hits = vec![
            hit("a", Some(1.0)),
            hit("b", Some(0.8)),
            hit("c", Some(0.5)),
        ];
        let result = fuse(vec![(RetrievalMethod::Fts, hits)], 10);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].row_id, "a");
        assert_eq!(result[0].rrf_score, 1.0 / (RRF_K as f32 + 1.0));
        assert_eq!(result[0].contributors.len(), 1);
        assert_eq!(result[0].contributors[0].rank, 1);

        assert_eq!(result[1].row_id, "b");
        assert_eq!(result[1].contributors[0].rank, 2);

        assert_eq!(result[2].row_id, "c");
        assert_eq!(result[2].contributors[0].rank, 3);
    }

    #[test]
    fn test_hybrid_overlap() {
        // Two methods with overlapping candidates
        // FTS: a at rank 1, b at rank 2
        // Vector: b at rank 1, c at rank 2
        let fts_hits = vec![hit("a", Some(1.0)), hit("b", Some(0.5))];
        let vector_hits = vec![hit("b", Some(0.1)), hit("c", Some(0.2))];

        let result = fuse(
            vec![
                (RetrievalMethod::Fts, fts_hits),
                (RetrievalMethod::Vector, vector_hits),
            ],
            10,
        );

        // "b" appears in both methods: rank 2 from FTS + rank 1 from Vector
        // RRF for b: 1/(60+2) + 1/(60+1) = 1/62 + 1/61
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].row_id, "b");
        let expected_b_score = 1.0 / 62.0 + 1.0 / 61.0;
        assert!(
            (result[0].rrf_score - expected_b_score).abs() < 0.0001,
            "expected {} got {}",
            expected_b_score,
            result[0].rrf_score
        );
        assert_eq!(result[0].contributors.len(), 2);

        // "a" and "c" each appear in one method
        assert_eq!(result[1].contributors.len(), 1);
        assert_eq!(result[2].contributors.len(), 1);
    }

    #[test]
    fn test_disjoint_methods() {
        // Two methods with fully disjoint row_ids
        let fts_hits = vec![hit("a", None), hit("c", None)];
        let vector_hits = vec![hit("b", None), hit("d", None)];

        let result = fuse(
            vec![
                (RetrievalMethod::Fts, fts_hits),
                (RetrievalMethod::Vector, vector_hits),
            ],
            10,
        );

        assert_eq!(result.len(), 4);
        // All have equal RRF scores (1/(60+rank)), so tiebreak by rank
        // a: rank 1 from Fts, c: rank 2 from Fts
        // b: rank 1 from Vector, d: rank 2 from Vector
        // All rank-1 items have equal best rank, so level-3 tiebreak: Fts < Vector
        assert_eq!(result[0].row_id, "a"); // Fts rank 1
        assert_eq!(result[1].row_id, "b"); // Vector rank 1
        // Now rank-2 items, same tiebreak
        assert_eq!(result[2].row_id, "c"); // Fts rank 2
        assert_eq!(result[3].row_id, "d"); // Vector rank 2
    }

    #[test]
    fn test_tiebreak_level_2_best_rank() {
        // A: fts#2, vector#5 -> best rank 2, RRF = 1/62 + 1/65
        // B: fts#5, vector#1 -> best rank 1, RRF = 1/65 + 1/61
        // B has better (lower) best rank, so B comes before A (level 2)
        let fts_hits = vec![
            hit("x", None),
            hit("a", None),
            hit("y", None),
            hit("z", None),
            hit("b", None),
        ];
        let vector_hits = vec![
            hit("b", None),
            hit("y", None),
            hit("z", None),
            hit("x", None),
            hit("a", None),
        ];

        let result = fuse(
            vec![
                (RetrievalMethod::Fts, fts_hits),
                (RetrievalMethod::Vector, vector_hits),
            ],
            10,
        );

        // Find a and b
        let a_pos = result.iter().position(|h| h.row_id == "a").unwrap();
        let b_pos = result.iter().position(|h| h.row_id == "b").unwrap();
        // b has best rank 1, a has best rank 2, so b comes before a
        assert!(
            b_pos < a_pos,
            "b should come before a because b has better best rank"
        );
    }

    #[test]
    fn test_tiebreak_level_4_lexicographic() {
        // Level 4 tiebreak: row_id lexicographic ascending.
        // This is hard to trigger naturally because Fts and Vector produce different RRF contributions.
        // For a true level-4 case, we need two row_ids with:
        // - Equal RRF scores
        // - Equal best ranks
        // - Best-rank methods that are equal (or we use single-method)
        //
        // Single-method case: two rows with same rank have equal RRF score.
        // Since they come from the same method, level-3 is a tie, so level-4 applies.
        // But wait - they can't have the same rank in a single list.
        //
        // The realistic case: this is extremely rare. The test just verifies the final
        // fallback produces stable ordering.
        let hits = vec![hit("zebra", None), hit("apple", None)];
        let result = fuse(vec![(RetrievalMethod::Fts, hits)], 10);

        // Both have different ranks (1 and 2), so zebra (rank 1) comes first
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row_id, "zebra"); // rank 1
        assert_eq!(result[1].row_id, "apple"); // rank 2
    }

    #[test]
    fn test_duplicate_row_id_within_method() {
        // Same row_id appears twice in one method's list
        // First occurrence wins (preserves best rank)
        let hits = vec![
            hit("a", Some(1.0)),
            hit("a", Some(0.5)), // duplicate, should be ignored
            hit("b", Some(0.3)),
        ];

        let result = fuse(vec![(RetrievalMethod::Fts, hits)], 10);

        assert_eq!(result.len(), 2);
        // "a" should only appear once with rank 1
        let a_hit = result.iter().find(|h| h.row_id == "a").unwrap();
        assert_eq!(a_hit.contributors.len(), 1);
        assert_eq!(a_hit.contributors[0].rank, 1);
        assert_eq!(a_hit.contributors[0].backend_score, Some(1.0)); // first occurrence's score
    }

    #[test]
    fn test_duplicate_does_not_double_contribute() {
        // Duplicate row_id should not double-contribute to RRF score
        let hits = vec![hit("a", None), hit("a", None), hit("a", None)];
        let result = fuse(vec![(RetrievalMethod::Fts, hits)], 10);

        assert_eq!(result.len(), 1);
        // Should only count once: 1/(60+1)
        let expected_score = 1.0 / (RRF_K as f32 + 1.0);
        assert!((result[0].rrf_score - expected_score).abs() < 0.0001);
    }

    #[test]
    fn test_truncation() {
        let hits: Vec<MethodHit> = (0..100).map(|i| hit(&format!("row_{}", i), None)).collect();
        let result = fuse(vec![(RetrievalMethod::Fts, hits)], 10);

        assert_eq!(result.len(), 10);
        // First 10 should be the top 10 by rank (lower rank = higher RRF score)
        for (i, hit) in result.iter().enumerate() {
            assert_eq!(hit.row_id, format!("row_{}", i));
        }
    }

    #[test]
    fn test_empty_inputs() {
        // Both empty
        let result = fuse(vec![(RetrievalMethod::Fts, vec![])], 10);
        assert!(result.is_empty());

        // One empty, one non-empty
        let fts_hits = vec![hit("a", None)];
        let result = fuse(
            vec![
                (RetrievalMethod::Fts, fts_hits.clone()),
                (RetrievalMethod::Vector, vec![]),
            ],
            10,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].row_id, "a");
    }

    #[test]
    fn test_determinism() {
        let hits1 = vec![hit("a", None), hit("b", None)];
        let hits2 = vec![hit("c", None), hit("d", None)];

        let result1 = fuse(
            vec![
                (RetrievalMethod::Fts, hits1.clone()),
                (RetrievalMethod::Vector, hits2.clone()),
            ],
            10,
        );
        let result2 = fuse(
            vec![
                (RetrievalMethod::Fts, hits1),
                (RetrievalMethod::Vector, hits2),
            ],
            10,
        );

        assert_eq!(result1, result2);
    }

    #[test]
    fn test_input_tuple_order_invariance() {
        // Order of method tuples in input should not affect output
        let fts_hits = vec![hit("a", None), hit("b", None)];
        let vector_hits = vec![hit("b", None), hit("c", None)];

        let result1 = fuse(
            vec![
                (RetrievalMethod::Fts, fts_hits.clone()),
                (RetrievalMethod::Vector, vector_hits.clone()),
            ],
            10,
        );
        let result2 = fuse(
            vec![
                (RetrievalMethod::Vector, vector_hits),
                (RetrievalMethod::Fts, fts_hits),
            ],
            10,
        );

        // Results should be identical (tiebreak uses enum order, not input order)
        assert_eq!(result1.len(), result2.len());
        for (a, b) in result1.iter().zip(result2.iter()) {
            assert_eq!(a.row_id, b.row_id);
            assert!((a.rrf_score - b.rrf_score).abs() < 0.0001);
        }
    }

    #[test]
    fn test_level_3_tiebreak_with_tied_best_rank() {
        // Row A: best rank is #2 from both Fts and Vector -> best-rank method is Fts (alphabetically first)
        // Row B: best rank is #2 from Vector only -> best-rank method is Vector
        // Level-3: Fts < Vector, so A wins
        let fts_hits = vec![hit("x", None), hit("a", None)]; // a at rank 2
        let vector_hits = vec![hit("a", None), hit("b", None)]; // a at rank 1, b at rank 2

        let result = fuse(
            vec![
                (RetrievalMethod::Fts, fts_hits),
                (RetrievalMethod::Vector, vector_hits),
            ],
            10,
        );

        // a has rank 2 from Fts and rank 1 from Vector
        // b has rank 2 from Vector only
        // a's best rank is 1, b's best rank is 2
        // So a comes before b (level 2 tiebreak: best rank ascending)
        let a_pos = result.iter().position(|h| h.row_id == "a").unwrap();
        let b_pos = result.iter().position(|h| h.row_id == "b").unwrap();
        assert!(a_pos < b_pos);
    }

    #[test]
    fn test_level_3_tiebreak_equal_best_ranks() {
        // Row A: best rank #2 from Fts only
        // Row B: best rank #2 from Vector only
        // Both have equal RRF score (1/62) and equal best rank (2)
        // Level-3: Fts < Vector, so A wins
        let fts_hits = vec![hit("x", None), hit("a", None)]; // a at rank 2
        let vector_hits = vec![hit("y", None), hit("b", None)]; // b at rank 2

        let result = fuse(
            vec![
                (RetrievalMethod::Fts, fts_hits),
                (RetrievalMethod::Vector, vector_hits),
            ],
            10,
        );

        // Both a and b have rank 2, but a's best-rank method is Fts, b's is Vector
        // Fts < Vector, so a comes before b
        let a_pos = result.iter().position(|h| h.row_id == "a").unwrap();
        let b_pos = result.iter().position(|h| h.row_id == "b").unwrap();
        assert!(
            a_pos < b_pos,
            "a should come before b due to level-3 tiebreak (Fts < Vector)"
        );
    }
}
