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
mod tests;
