//! Search decision-rule evaluation.
//!
//! Purpose: Determine which retrieval methods to query based on label metadata and CLI flags.
//! Edit here when: Changing decision rules, adding new retrieval methods.
//! Do not edit here for: Rendering warnings (see app/search/), storage operations.
//!
//! ## Decision table
//!
//! | Active subset size | Source state                  | Behavior                     |
//! |--------------------|-------------------------------|------------------------------|
//! | 0                  | n/a                           | Error (empty/incomplete)     |
//! | 1                  | n/a                           | Use that method              |
//! | 2+                 | all sources equal             | Hybrid (RRF)                 |
//! | 2+                 | sources disagree              | Error (sources disagree)     |
//!
//! Active subset is computed from the persistent selection (methods with non-NULL source)
//! filtered by: (a) explicit `--retrieval` flags if provided, or (b) removing incomplete
//! methods for the no-flag path.

use std::collections::BTreeSet;

use crate::engine::retrieval::RetrievalMethod;
use crate::engine::storage::LabelMetadataRow;
use crate::engine::warning::DecisionWarning;

/// The decision outcome from evaluating label metadata and requested methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Use a single retrieval method.
    SingleMethod {
        method: RetrievalMethod,
        /// Warnings collected during decision evaluation (e.g., incomplete method).
        decision_warnings: Vec<DecisionWarning>,
    },
    /// Use hybrid retrieval (RRF fusion).
    Hybrid {
        methods: BTreeSet<RetrievalMethod>,
        /// Warnings collected during decision evaluation.
        decision_warnings: Vec<DecisionWarning>,
    },
    /// Decision resulted in an error.
    Error(DecisionError),
}

/// Error cases from decision evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionError {
    /// The label has no retrieval methods in its selection.
    EmptySelection,
    /// All in-selection methods are incomplete (no explicit --retrieval was given).
    AllInSelectionIncomplete {
        incomplete_methods: BTreeSet<RetrievalMethod>,
    },
    /// Two or more methods have different source values.
    SourcesDisagree {
        vector_source: String,
        fts_source: String,
    },
    /// Explicit --retrieval requested a method not in the selection.
    MethodNotInSelection { method: RetrievalMethod },
    /// Explicit --retrieval requested multiple methods but some are not in selection.
    /// The set contains the methods that were requested but not available.
    MethodsNotInSelection { methods: BTreeSet<RetrievalMethod> },
}

/// Evaluate the decision table and return which retrieval methods to use.
///
/// ## Arguments
///
/// * `metadata` - The label's metadata row containing per-method source/complete state.
/// * `requested` - The set of methods requested via `--retrieval` flags, or `None` for default.
///
/// ## Returns
///
/// A `Decision` indicating single-method, hybrid, or error, along with any warnings.
///
/// ## Warning behavior
///
/// - For explicit `--retrieval X` against an incomplete method: warning is attached to the
///   `SingleMethod` variant.
/// - For no-flag default with incomplete methods: incomplete methods are filtered from the
///   active subset, warnings are attached to the result, and if all methods are incomplete,
///   returns `Decision::Error(AllInSelectionIncomplete)`.
pub fn decide(
    metadata: &LabelMetadataRow,
    requested: Option<BTreeSet<RetrievalMethod>>,
) -> Decision {
    // Step 1: Compute the persistent selection (methods with non-NULL source)
    let mut persistent_selection: BTreeSet<RetrievalMethod> = BTreeSet::new();
    if metadata.vector_source.is_some() {
        persistent_selection.insert(RetrievalMethod::Vector);
    }
    if metadata.fts_source.is_some() {
        persistent_selection.insert(RetrievalMethod::Fts);
    }

    // Step 2: Apply explicit --retrieval filter if provided
    let candidate_subset = if let Some(ref requested_set) = requested {
        // Check for methods not in selection
        let not_in_selection: BTreeSet<RetrievalMethod> = requested_set
            .difference(&persistent_selection)
            .copied()
            .collect();

        if !not_in_selection.is_empty() {
            if not_in_selection.len() == 1 {
                let method = not_in_selection.iter().next().copied().unwrap();
                return Decision::Error(DecisionError::MethodNotInSelection { method });
            } else {
                return Decision::Error(DecisionError::MethodsNotInSelection {
                    methods: not_in_selection,
                });
            }
        }

        requested_set.clone()
    } else {
        persistent_selection.clone()
    };

    // Step 3: Check for empty selection
    if candidate_subset.is_empty() {
        return Decision::Error(DecisionError::EmptySelection);
    }

    // Step 4: Collect incomplete methods (in selection but not complete)
    let incomplete_methods: BTreeSet<RetrievalMethod> = candidate_subset
        .iter()
        .filter(|&&method| match method {
            RetrievalMethod::Vector => !metadata.vector_complete,
            RetrievalMethod::Fts => !metadata.fts_complete,
        })
        .copied()
        .collect();

    // Step 5: Compute active subset
    // For explicit --retrieval: keep all requested methods (even incomplete ones)
    // For no-flag: filter out incomplete methods
    let (active_subset, decision_warnings): (BTreeSet<RetrievalMethod>, Vec<DecisionWarning>) =
        if requested.is_some() {
            // Explicit --retrieval: use the candidate subset as-is, attach warnings for incomplete
            let warnings = incomplete_methods
                .iter()
                .map(|&m| DecisionWarning::IncompleteMethod { method: m })
                .collect();
            (candidate_subset, warnings)
        } else {
            // No-flag: filter out incomplete methods, attach warnings for filtered ones
            let warnings = incomplete_methods
                .iter()
                .map(|&m| DecisionWarning::IncompleteMethod { method: m })
                .collect();
            let active = candidate_subset
                .difference(&incomplete_methods)
                .copied()
                .collect();
            (active, warnings)
        };

    // Step 6: Check for empty active subset (all methods were incomplete)
    if active_subset.is_empty() {
        return Decision::Error(DecisionError::AllInSelectionIncomplete { incomplete_methods });
    }

    // Step 7: Check for source agreement (only matters for size 2+)
    if active_subset.len() >= 2 {
        let vector_source = metadata.vector_source.as_ref();
        let fts_source = metadata.fts_source.as_ref();

        // Both should be Some since both are in active_subset
        // (active_subset is a subset of candidate_subset which is a subset of persistent_selection)
        match (vector_source, fts_source) {
            (Some(v), Some(f)) if v != f => {
                return Decision::Error(DecisionError::SourcesDisagree {
                    vector_source: v.clone(),
                    fts_source: f.clone(),
                });
            }
            _ => {}
        }
    }

    // Step 8: Dispatch based on active subset size
    if active_subset.len() == 1 {
        let method = active_subset.iter().next().copied().unwrap();
        Decision::SingleMethod {
            method,
            decision_warnings,
        }
    } else {
        Decision::Hybrid {
            methods: active_subset,
            decision_warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_metadata() -> LabelMetadataRow {
        LabelMetadataRow {
            label_id: "test-catalog:main".to_string(),
            catalog: "test-catalog".to_string(),
            label: "main".to_string(),
            source_kind: "git-commit".to_string(),
            vector_source: Some("abc123".to_string()),
            vector_complete: true,
            fts_source: Some("abc123".to_string()),
            fts_complete: true,
            updated_at_unix_secs: 1700000000,
        }
    }

    // =========================================================================
    // Size 0 cases
    // =========================================================================

    #[test]
    fn test_empty_selection() {
        let mut metadata = valid_metadata();
        metadata.vector_source = None;
        metadata.fts_source = None;

        let result = decide(&metadata, None);
        assert_eq!(result, Decision::Error(DecisionError::EmptySelection));
    }

    #[test]
    fn test_all_in_selection_incomplete() {
        let mut metadata = valid_metadata();
        metadata.vector_complete = false;
        metadata.fts_complete = false;

        let result = decide(&metadata, None);
        let mut expected_incomplete = BTreeSet::new();
        expected_incomplete.insert(RetrievalMethod::Vector);
        expected_incomplete.insert(RetrievalMethod::Fts);
        assert_eq!(
            result,
            Decision::Error(DecisionError::AllInSelectionIncomplete {
                incomplete_methods: expected_incomplete
            })
        );
    }

    // =========================================================================
    // Size 1 cases
    // =========================================================================

    #[test]
    fn test_single_method_vector() {
        let mut metadata = valid_metadata();
        metadata.fts_source = None; // Only vector in selection

        let result = decide(&metadata, None);
        assert_eq!(
            result,
            Decision::SingleMethod {
                method: RetrievalMethod::Vector,
                decision_warnings: vec![],
            }
        );
    }

    #[test]
    fn test_single_method_fts() {
        let mut metadata = valid_metadata();
        metadata.vector_source = None; // Only FTS in selection

        let result = decide(&metadata, None);
        assert_eq!(
            result,
            Decision::SingleMethod {
                method: RetrievalMethod::Fts,
                decision_warnings: vec![],
            }
        );
    }

    #[test]
    fn test_single_method_with_explicit_request() {
        let metadata = valid_metadata();
        let mut requested = BTreeSet::new();
        requested.insert(RetrievalMethod::Vector);

        let result = decide(&metadata, Some(requested));
        assert_eq!(
            result,
            Decision::SingleMethod {
                method: RetrievalMethod::Vector,
                decision_warnings: vec![],
            }
        );
    }

    // =========================================================================
    // Size 2+ cases
    // =========================================================================

    #[test]
    fn test_hybrid_sources_equal() {
        let metadata = valid_metadata(); // Both sources = "abc123"

        let result = decide(&metadata, None);
        let mut expected_methods = BTreeSet::new();
        expected_methods.insert(RetrievalMethod::Fts);
        expected_methods.insert(RetrievalMethod::Vector);
        assert_eq!(
            result,
            Decision::Hybrid {
                methods: expected_methods,
                decision_warnings: vec![],
            }
        );
    }

    #[test]
    fn test_hybrid_explicit_request() {
        let metadata = valid_metadata();
        let mut requested = BTreeSet::new();
        requested.insert(RetrievalMethod::Fts);
        requested.insert(RetrievalMethod::Vector);

        let result = decide(&metadata, Some(requested));
        let mut expected_methods = BTreeSet::new();
        expected_methods.insert(RetrievalMethod::Fts);
        expected_methods.insert(RetrievalMethod::Vector);
        assert_eq!(
            result,
            Decision::Hybrid {
                methods: expected_methods,
                decision_warnings: vec![],
            }
        );
    }

    #[test]
    fn test_sources_disagree() {
        let mut metadata = valid_metadata();
        metadata.vector_source = Some("commit_a".to_string());
        metadata.fts_source = Some("commit_b".to_string());

        let result = decide(&metadata, None);
        assert_eq!(
            result,
            Decision::Error(DecisionError::SourcesDisagree {
                vector_source: "commit_a".to_string(),
                fts_source: "commit_b".to_string(),
            })
        );
    }

    // =========================================================================
    // Method not in selection cases
    // =========================================================================

    #[test]
    fn test_method_not_in_selection_single() {
        let mut metadata = valid_metadata();
        metadata.fts_source = None; // FTS not in selection

        let mut requested = BTreeSet::new();
        requested.insert(RetrievalMethod::Fts);

        let result = decide(&metadata, Some(requested));
        assert_eq!(
            result,
            Decision::Error(DecisionError::MethodNotInSelection {
                method: RetrievalMethod::Fts
            })
        );
    }

    #[test]
    fn test_methods_not_in_selection_multiple() {
        let mut metadata = valid_metadata();
        metadata.vector_source = None;
        metadata.fts_source = None;

        let mut requested = BTreeSet::new();
        requested.insert(RetrievalMethod::Fts);
        requested.insert(RetrievalMethod::Vector);

        let result = decide(&metadata, Some(requested));
        let mut expected = BTreeSet::new();
        expected.insert(RetrievalMethod::Fts);
        expected.insert(RetrievalMethod::Vector);
        assert_eq!(
            result,
            Decision::Error(DecisionError::MethodsNotInSelection { methods: expected })
        );
    }

    // =========================================================================
    // Incomplete method warning cases
    // =========================================================================

    #[test]
    fn test_explicit_request_incomplete_method() {
        let mut metadata = valid_metadata();
        metadata.vector_complete = false;

        let mut requested = BTreeSet::new();
        requested.insert(RetrievalMethod::Vector);

        let result = decide(&metadata, Some(requested));
        assert_eq!(
            result,
            Decision::SingleMethod {
                method: RetrievalMethod::Vector,
                decision_warnings: vec![DecisionWarning::IncompleteMethod {
                    method: RetrievalMethod::Vector
                }],
            }
        );
    }

    #[test]
    fn test_no_flag_filters_incomplete_method() {
        let mut metadata = valid_metadata();
        metadata.vector_complete = false; // Vector incomplete

        let result = decide(&metadata, None);
        // Vector is filtered out, FTS remains
        assert_eq!(
            result,
            Decision::SingleMethod {
                method: RetrievalMethod::Fts,
                decision_warnings: vec![DecisionWarning::IncompleteMethod {
                    method: RetrievalMethod::Vector
                }],
            }
        );
    }

    #[test]
    fn test_no_flag_one_incomplete_filter_to_single() {
        let mut metadata = valid_metadata();
        metadata.fts_complete = false; // FTS incomplete

        let result = decide(&metadata, None);
        // FTS is filtered out, Vector remains
        assert_eq!(
            result,
            Decision::SingleMethod {
                method: RetrievalMethod::Vector,
                decision_warnings: vec![DecisionWarning::IncompleteMethod {
                    method: RetrievalMethod::Fts
                }],
            }
        );
    }

    #[test]
    fn test_explicit_request_both_incomplete() {
        let mut metadata = valid_metadata();
        metadata.vector_complete = false;
        metadata.fts_complete = false;

        let mut requested = BTreeSet::new();
        requested.insert(RetrievalMethod::Fts);
        requested.insert(RetrievalMethod::Vector);

        let result = decide(&metadata, Some(requested));
        // With explicit --retrieval, incomplete methods are NOT filtered out
        let mut expected_methods = BTreeSet::new();
        expected_methods.insert(RetrievalMethod::Fts);
        expected_methods.insert(RetrievalMethod::Vector);
        assert_eq!(
            result,
            Decision::Hybrid {
                methods: expected_methods,
                decision_warnings: vec![
                    DecisionWarning::IncompleteMethod {
                        method: RetrievalMethod::Fts
                    },
                    DecisionWarning::IncompleteMethod {
                        method: RetrievalMethod::Vector
                    },
                ],
            }
        );
    }
}
