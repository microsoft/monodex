//! Purpose: Test suite for reciprocal rank fusion.
//! Edit here when: Adding or modifying tests for reciprocal rank fusion.
//! Do not edit here for: Fusion implementation (see `../fusion.rs`).

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
fn test_single_method_two_hits_ordered_by_rank() {
    // Single-method case: two hits ordered by their input rank.
    // Verifies that rank-1 comes before rank-2.
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
fn test_exact_score_comparison_within_epsilon() {
    // Test that RRF scores very close together (within f32::EPSILON) are still
    // ordered correctly by their exact values, not treated as equal.
    //
    // Case from spot-check: (fts=4, vec=32) vs (fts=1, vec=39)
    // RRF score for (4, 32): 1/(60+4) + 1/(60+32) = 1/64 + 1/92 ≈ 0.02649456
    // RRF score for (1, 39): 1/(60+1) + 1/(60+39) = 1/61 + 1/99 ≈ 0.02649445
    // Gap: ~1.12e-7, which is below f32::EPSILON (~1.19e-7)
    //
    // The (fts=4, vec=32) row should come first because its score is higher.
    let fts_hits = vec![
        hit("x", None),     // rank 1
        hit("y", None),     // rank 2
        hit("z", None),     // rank 3
        hit("row_a", None), // rank 4
    ];
    let vector_hits: Vec<MethodHit> = (1..=39)
        .map(|i| {
            if i == 32 {
                hit("row_a", None) // row_a at rank 32
            } else if i == 39 {
                hit("row_b", None) // row_b at rank 39
            } else {
                hit(&format!("fill_{}", i), None)
            }
        })
        .collect();

    let result = fuse(
        vec![
            (RetrievalMethod::Fts, fts_hits),
            (RetrievalMethod::Vector, vector_hits),
        ],
        50,
    );

    // row_a has (fts=4, vec=32) -> higher RRF score
    // row_b has (fts=1, vec=39) -> lower RRF score
    // row_a should appear before row_b
    let a_pos = result.iter().position(|h| h.row_id == "row_a").unwrap();
    let b_pos = result.iter().position(|h| h.row_id == "row_b").unwrap();
    assert!(
        a_pos < b_pos,
        "row_a (fts=4, vec=32, score={}) should come before row_b (fts=1, vec=39, score={})",
        result[a_pos].rrf_score,
        result[b_pos].rrf_score
    );
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
