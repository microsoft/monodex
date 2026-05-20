//! Purpose: Recursive search for split points at AST boundaries; descent into nested scopes when no shallow split fits the budget.
//! Edit here when: Changing split scope definitions, split heuristics, or descent logic.
//! Do not edit here for: Chunk size targets (see `types.rs`), quality scoring (see `scoring.rs`), AST node metadata (see `node_analysis.rs`).

use super::debug::PartitionDebug;
use super::node_analysis::get_meaningful_children;
use super::types::SplitResult;
use tree_sitter::Node;

/// Estimate sizes of the two chunks that would result from splitting at a given line.
///
/// Uses line counts to proportionally divide the character budget.
fn estimate_split_sizes(
    chunk_size: usize,
    start_line: usize,
    split_line: usize,
    end_line: usize,
) -> (usize, usize) {
    let lines_before = split_line - start_line + 1;
    let total_lines = end_line - start_line + 1;
    let estimated_first_size = (chunk_size * lines_before) / total_lines;
    let estimated_second_size = chunk_size - estimated_first_size;
    (estimated_first_size, estimated_second_size)
}

/// Check if a node is a split scope - direct children define split boundaries.
pub(crate) fn is_split_scope(kind: &str) -> bool {
    matches!(
        kind,
        "program" | "source_file" |
        "class_body" | "declaration_list" | "object_type" |
        "interface_body" |  // Interface body (contains property_signature children)
        "statement_block" | "switch_body" |
        "object" |  // Object literals (contain 'pair' children)
        "jsx_element" | "jsx_fragment" // JSX elements can be split at child boundaries
    )
}

/// Check if a node is a transparent conduit - pass through to nested scopes.
pub(crate) fn is_transparent_conduit(kind: &str) -> bool {
    matches!(
        kind,
        "export_statement" |
        // Declaration containers (hold class_body, object_type, etc.)
        "class_declaration" | "abstract_class_declaration" | "interface_declaration" |
        "type_alias_declaration" | "enum_declaration" |
        // Function declarations (hold statement_block with nested functions)
        "function_declaration" | "generator_function_declaration" |
        "method_definition" | "arrow_function" |
        "function_expression" |  // Named or anonymous function passed as argument (callback)
        "if_statement" | "try_statement" | "catch_clause" |
        "else_clause" |  // else clause of if statements (contains statement_block or nested if)
        "for_statement" | "for_in_statement" | "for_of_statement" |
        "while_statement" | "do_statement" |
        "switch_statement" | "switch_case" |
        "return_statement" | "throw_statement" |
        "expression_statement" |
        // Variable declarations may contain object literals with properties
        "lexical_declaration" | "variable_declarator" |
        // Expression wrappers that may contain nested scopes
        "await_expression" | "new_expression" | "arguments" | "call_expression" |
        "member_expression" |  // e.g., `new Promise(...).finally(...)` - need to descend to find Promise executor
        "as_expression" |  // `expr as const` wraps object literals
        // Object literal pairs may contain nested functions/objects
        "pair" |  // Object property with function value that needs splitting
        // JSX expression containers (wrap expressions inside JSX)
        "jsx_expression"
    )
}

/// Find the best split point for an oversized chunk using scope-based approach.
#[allow(clippy::too_many_arguments)]
pub(crate) fn find_best_split(
    root: Node,
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    min_chunk_size: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> SplitResult {
    debug.log_split_attempt(start_line, end_line, chunk_size);

    // Find the shallowest split scope that spans this chunk
    let initial_scope = find_shallowest_split_scope(root, start_line, end_line, source, debug);
    debug.log_scope(
        "Initial",
        initial_scope.kind(),
        initial_scope.start_position().row + 1,
        initial_scope.end_position().row + 1,
    );

    // Track the least-bad AST split seen during descent
    let mut least_bad_split: Option<(usize, usize)> = None;
    let ideal_first_size = chunk_size / 2;

    // Walk the chain of nested split scopes
    let mut current_scope = Some(initial_scope);
    let mut iteration = 0;

    while let Some(scope) = current_scope {
        iteration += 1;
        debug.log(&format!("--- Iteration {} ---", iteration));

        // Get candidate boundaries from this scope's direct children
        let candidates = get_scope_candidates(scope, source, start_line, end_line, debug);
        debug.log_candidates(&candidates);

        if !candidates.is_empty() {
            // Check if this scope yields a usable partition
            if let Some(split_line) = find_usable_split(
                &candidates,
                start_line,
                end_line,
                chunk_size,
                min_chunk_size,
            ) {
                debug.log_split_decision("USABLE SPLIT", Some(split_line));
                return SplitResult::Split(split_line);
            }
            debug.log_split_decision("No usable split (min_chunk constraint)", None);

            // No usable partition - record the least-bad split from this scope
            // But only if it respects min_chunk_size (don't record tiny splits)
            for &split_line in &candidates {
                let (estimated_first_size, estimated_second_size) =
                    estimate_split_sizes(chunk_size, start_line, split_line, end_line);

                // Compute badness for this split
                // Note: We include ALL candidates, even those that create tiny chunks.
                // A degraded AST split may be preferable operationally to fallback,
                // but it is still a quality failure distinct from a successful AST split.
                // The badness calculation already penalizes small chunks heavily.
                let badness = compute_split_badness(
                    split_line,
                    start_line,
                    end_line,
                    chunk_size,
                    ideal_first_size,
                );

                // Log the candidate with size info
                let size_note = if estimated_first_size < min_chunk_size
                    || estimated_second_size < min_chunk_size
                {
                    " (WARNING: creates tiny chunk)".to_string()
                } else {
                    String::new()
                };
                debug.log(&format!(
                    "  Candidate line {} -> sizes ({}, {}) badness {}{}",
                    split_line, estimated_first_size, estimated_second_size, badness, size_note
                ));

                if least_bad_split.is_none_or(|(_, b)| badness < b) {
                    least_bad_split = Some((split_line, badness));
                }
            }
        }

        // Descend to a nested split scope
        current_scope = find_nested_split_scope(
            scope,
            start_line,
            end_line,
            chunk_size,
            min_chunk_size,
            source,
            debug,
        );
    }

    // No usable partition found - use least-bad AST split if available
    // A degraded AST split may be preferable operationally to fallback,
    // but it is still a quality failure distinct from a successful AST split.
    if let Some((split_line, badness)) = least_bad_split {
        // Check if this is a degraded split (creates tiny chunks)
        let (estimated_first_size, estimated_second_size) =
            estimate_split_sizes(chunk_size, start_line, split_line, end_line);

        if estimated_first_size < min_chunk_size || estimated_second_size < min_chunk_size {
            debug.log_split_decision(
                &format!("DEGRADED AST SPLIT (badness {}, tiny chunk)", badness),
                Some(split_line),
            );
            return SplitResult::DegradedSplit(split_line);
        } else {
            debug.log_split_decision(
                &format!("LEAST-BAD AST SPLIT (badness {})", badness),
                Some(split_line),
            );
            return SplitResult::Split(split_line);
        }
    }

    // No AST split at all - use line-based fallback
    let mid_line = start_line + (end_line - start_line) / 2;
    if mid_line > start_line && mid_line < end_line {
        debug.log_split_decision("FALLBACK (no AST candidates)", Some(mid_line));
        SplitResult::Fallback(mid_line)
    } else {
        debug.log_split_decision("CANNOT SPLIT (too small)", None);
        SplitResult::CannotSplit
    }
}

/// Find the shallowest split scope that spans the given line range.
pub(crate) fn find_shallowest_split_scope<'a>(
    node: Node<'a>,
    start_line: usize,
    end_line: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> Node<'a> {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    debug_assert!(
        node_start <= start_line && node_end >= end_line,
        "find_shallowest_split_scope called with node that doesn't span the range: node {}-{}, range {}-{}",
        node_start,
        node_end,
        start_line,
        end_line
    );

    // If this is a split scope, return it (shallowest wins)
    if is_split_scope(node.kind()) {
        return node;
    }

    // If transparent conduit, pass through to the child that spans the range
    if is_transparent_conduit(node.kind()) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_start = child.start_position().row + 1;
            let child_end = child.end_position().row + 1;
            if child_start <= start_line && child_end >= end_line {
                return find_shallowest_split_scope(child, start_line, end_line, source, debug);
            }
        }
    }

    // Check for single meaningful child to descend into
    let meaningful = get_meaningful_children(node, source, debug);
    if meaningful.len() == 1 {
        let child = meaningful[0];
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;
        if child_start <= start_line && child_end >= end_line {
            return find_shallowest_split_scope(child, start_line, end_line, source, debug);
        }
    }

    node
}

/// Find a nested split scope to descend into.
/// Selects the best child (split scope or transparent conduit) and delegates to
/// `find_deepest_split_scope()` to validate viability before returning.
/// Only returns a scope that has at least one viable candidate (meets min_chunk_size).
pub(crate) fn find_nested_split_scope<'a>(
    node: Node<'a>,
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    min_chunk_size: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let mut best_child: Option<Node<'a>> = None;
    let mut best_score = i32::MIN;

    for child in node.children(&mut cursor) {
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;

        // Must overlap the chunk range
        if child_end < start_line || child_start > end_line {
            continue;
        }

        // Check if this child is a split scope or transparent conduit
        if is_split_scope(child.kind()) || is_transparent_conduit(child.kind()) {
            let overlap_start = child_start.max(start_line);
            let overlap_end = child_end.min(end_line);
            let overlap = (overlap_end - overlap_start) as i32;

            // Prefer children that are split scopes and centered in the range
            let center_score = if is_split_scope(child.kind()) {
                1000
            } else {
                0
            };
            let total_score = center_score + overlap;

            if total_score > best_score {
                best_score = total_score;
                best_child = Some(child);
            }
        }
    }

    // If we found a child, validate it before returning
    if let Some(child) = best_child {
        debug.log(&format!(
            "Descending to child '{}' at lines {}-{}",
            child.kind(),
            child.start_position().row + 1,
            child.end_position().row + 1
        ));

        // Both transparent conduits AND split scopes must pass through viability check
        if is_transparent_conduit(child.kind()) || is_split_scope(child.kind()) {
            // find_deepest_split_scope will validate viability before returning
            return find_deepest_split_scope(
                child,
                start_line,
                end_line,
                chunk_size,
                min_chunk_size,
                source,
                debug,
            );
        }
    }

    None
}

/// Find a usable split scope within a transparent conduit chain.
/// Descends through layers of transparent conduits until finding a split scope
/// that has meaningful children in the given line range AND yields at least one
/// viable split candidate (not just any split scope, but one that produces healthy chunks).
pub(crate) fn find_deepest_split_scope<'a>(
    node: Node<'a>,
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    min_chunk_size: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> Option<Node<'a>> {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    // Must overlap the chunk range
    if node_end < start_line || node_start > end_line {
        return None;
    }

    // If this is a split scope, check if it has viable candidates
    if is_split_scope(node.kind()) {
        let children = get_meaningful_children(node, source, debug);
        let overlapping: Vec<Node> = children
            .into_iter()
            .filter(|child| {
                let child_start = child.start_position().row + 1;
                let child_end = child.end_position().row + 1;
                child_end >= start_line && child_start <= end_line
            })
            .collect();

        // Need at least 2 overlapping children to have split candidates
        if overlapping.len() >= 2 {
            // Generate candidate split lines (same logic as get_scope_candidates)
            let mut candidates: Vec<usize> = Vec::new();
            for i in 1..overlapping.len() {
                let prev_child = overlapping[i - 1];
                let split_line = prev_child.end_position().row + 1;
                if split_line >= start_line && split_line < end_line {
                    candidates.push(split_line);
                }
            }

            // Check if any candidate produces chunks that meet min_chunk_size
            let has_viable_candidate = candidates.iter().any(|&split_line| {
                let (estimated_first_size, estimated_second_size) =
                    estimate_split_sizes(chunk_size, start_line, split_line, end_line);
                estimated_first_size >= min_chunk_size && estimated_second_size >= min_chunk_size
            });

            if has_viable_candidate {
                debug.log(&format!(
                    "  -> Accepted split scope '{}' at lines {}-{} (has viable candidates)",
                    node.kind(),
                    node_start,
                    node_end
                ));
                return Some(node);
            } else {
                debug.log(&format!(
                    "  -> Rejected split scope '{}' at lines {}-{} (no viable candidates)",
                    node.kind(),
                    node_start,
                    node_end
                ));
            }
        }
    }

    // If transparent conduit or split scope without viable candidates, descend further
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;

        if child_end < start_line || child_start > end_line {
            continue;
        }

        // Recursively descend into transparent conduits or split scopes
        if (is_transparent_conduit(child.kind()) || is_split_scope(child.kind()))
            && let Some(deeper) = find_deepest_split_scope(
                child,
                start_line,
                end_line,
                chunk_size,
                min_chunk_size,
                source,
                debug,
            )
        {
            return Some(deeper);
        }
    }

    None
}

/// Get candidate split boundaries from a scope's direct children.
pub(crate) fn get_scope_candidates(
    scope: Node,
    source: &[u8],
    chunk_start: usize,
    chunk_end: usize,
    debug: &PartitionDebug,
) -> Vec<usize> {
    let children = get_meaningful_children(scope, source, debug);

    let overlapping: Vec<Node> = children
        .into_iter()
        .filter(|child| {
            let child_start = child.start_position().row + 1;
            let child_end = child.end_position().row + 1;
            child_end >= chunk_start && child_start <= chunk_end
        })
        .collect();

    if overlapping.len() < 2 {
        return Vec::new();
    }

    let mut candidates: Vec<usize> = Vec::new();
    for i in 1..overlapping.len() {
        let prev_child = overlapping[i - 1];
        let split_line = prev_child.end_position().row + 1;
        if split_line >= chunk_start && split_line < chunk_end {
            candidates.push(split_line);
        }
    }
    candidates
}

/// Find a usable split from the candidates.
pub(crate) fn find_usable_split(
    candidates: &[usize],
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    min_chunk_size: usize,
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }

    let ideal_first_size = chunk_size / 2;
    let mut best_split: Option<usize> = None;
    let mut best_distance = usize::MAX;

    for &split_line in candidates {
        let (estimated_first_size, estimated_second_size) =
            estimate_split_sizes(chunk_size, start_line, split_line, end_line);

        // Skip splits that create tiny chunks (below minimum size)
        // Note: We don't require chunks to be below target_size here,
        // because the greedy loop will split oversized chunks in subsequent iterations.
        if estimated_first_size < min_chunk_size || estimated_second_size < min_chunk_size {
            continue;
        }

        // Prefer splits closest to middle
        let distance = estimated_first_size.abs_diff(ideal_first_size);

        if distance < best_distance {
            best_distance = distance;
            best_split = Some(split_line);
        }
    }
    best_split
}

/// Compute badness score for a split (lower is better).
pub(crate) fn compute_split_badness(
    split_line: usize,
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    ideal_first_size: usize,
) -> usize {
    let (estimated_first_size, estimated_second_size) =
        estimate_split_sizes(chunk_size, start_line, split_line, end_line);

    let distance = estimated_first_size.abs_diff(ideal_first_size);

    // Add penalty for small chunks
    let tiny_penalty = if estimated_first_size < 500 || estimated_second_size < 500 {
        10000
    } else if estimated_first_size < 1000 || estimated_second_size < 1000 {
        5000
    } else {
        0
    };

    distance + tiny_penalty
}
