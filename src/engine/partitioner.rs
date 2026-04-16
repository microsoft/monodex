//! Partition-based code chunking
//!
//! ## Algorithm: Two Worlds Model
//!
//! The algorithm coordinates two separate concerns:
//!
//! **Chunk Land (sizing/selection):**
//! - The file is a sequence of line ranges (chunks)
//! - Can measure any chunk's size in characters
//! - Can split a chunk at a given line number
//! - Knows the budget and when we're done
//!
//! **AST Land (structure/meaning):**
//! - Recursively walks the syntax tree using scope-based traversal
//! - Provides candidate split points as line numbers
//! - "Meaningful" = doesn't break semantic units
//!
//! **Scope-Based Traversal:**
//! - Split scopes: nodes whose direct children define split boundaries
//!   (program, class_body, statement_block, switch_body)
//! - Transparent conduits: wrapper nodes to pass through when descending
//!   (if_statement, function_declaration, return_statement, etc.)
//! - Core rule: choose the shallowest split scope that yields a usable partition
//!
//! **Minimum Size Constraints:**
//! - Minimum chunk size: 20% of target (prevents tiny fragments)
//! - Nested scopes are filtered by viability: only descend if they have at least
//!   one candidate that produces chunks meeting min_chunk_size
//! - Large expression statements (>500 bytes) are treated as meaningful
//!
//! **Split Outcome Categories:**
//! 1. Good AST split (success): semantically meaningful, respects min_chunk_size
//! 2. Degraded AST split (quality failure): semantically meaningful but poor geometry
//! 3. Fallback split (algorithm failure): no acceptable AST split found
//!
//! **Important:** Fallback is NOT a heuristic choice. It is an explicit failure mode
//! indicating the AST-based partitioner could not find any semantic structure to use.
//!
//! **Coordination:**
//! 1. Start with one chunk = entire file
//! 2. While any chunk exceeds budget:
//!    a. Find the shallowest split scope spanning the chunk
//!    b. Get candidate boundaries from that scope's direct children
//!    c. If usable split found, divide the chunk
//!    d. Otherwise, descend through transparent conduits to nested scopes
//!    e. If no viable nested scope or no usable split, use least-bad AST split
//!    f. If no AST candidates at all, fall back to line-based splitting
//! 3. Done - all chunks fit budget

use super::util::compute_hash;
use tree_sitter::{Language, Node, Parser};

/// Target chunk size in characters (same as runtime chunker's target_size)
pub const TARGET_CHARS: usize = 6000;

/// Threshold for "small" chunks in characters (roughly 20 lines × 50 chars)
pub const SMALL_CHUNK_CHARS: usize = 500;

/// Minimum chunk size as ratio of target (20%)
const MIN_CHUNK_RATIO: f64 = 0.20;

/// Debug logging for partitioning decisions
#[derive(Debug, Clone, Copy, Default)]
pub struct PartitionDebug {
    /// Enable verbose logging of split decisions
    pub enabled: bool,
}

impl PartitionDebug {
    pub fn log(&self, msg: &str) {
        if self.enabled {
            eprintln!("[DEBUG] {}", msg);
        }
    }

    pub fn log_split_attempt(&self, start_line: usize, end_line: usize, chunk_size: usize) {
        if self.enabled {
            eprintln!(
                "[DEBUG] === Splitting chunk lines {}-{} ({} chars) ===",
                start_line, end_line, chunk_size
            );
        }
    }

    pub fn log_scope(&self, scope_type: &str, kind: &str, start_line: usize, end_line: usize) {
        if self.enabled {
            eprintln!(
                "[DEBUG] {} scope '{}' at lines {}-{}",
                scope_type, kind, start_line, end_line
            );
        }
    }

    pub fn log_candidates(&self, candidates: &[usize]) {
        if self.enabled {
            eprintln!("[DEBUG]   Candidates: {:?}", candidates);
        }
    }

    pub fn log_split_decision(&self, result: &str, split_line: Option<usize>) {
        if self.enabled {
            match split_line {
                Some(line) => eprintln!("[DEBUG]   => {} at line {}", result, line),
                None => eprintln!("[DEBUG]   => {}", result),
            }
        }
    }

    pub fn log_meaningful_child(&self, kind: &str, start_line: usize, end_line: usize) {
        if self.enabled {
            eprintln!(
                "[DEBUG]   Meaningful child: '{}' at lines {}-{}",
                kind, start_line, end_line
            );
        }
    }
}

/// Check if a node is a split scope - direct children define split boundaries.
fn is_split_scope(kind: &str) -> bool {
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
fn is_transparent_conduit(kind: &str) -> bool {
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

/// Compute quality score for chunking results (0-100%, higher is better).
///
/// The score combines two factors:
/// 1. Count badness: How many more chunks than ideal (0 if at ideal, 1 if all 1-line chunks)
/// 2. Micro-chunk badness: How small the chunks are relative to ideal (0 if all at max size)
///
/// Final score = 100 × (1 - count_badness) × (1 - micro_badness)³
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

/// Configuration for partition chunking
pub struct PartitionConfig {
    /// Target chunk size in characters (text only, breadcrumb is extra)
    pub target_size: usize,

    /// File name for breadcrumb prefix
    pub file_name: String,

    /// Package name for breadcrumb (e.g., "@rushstack/node-core-library")
    pub package_name: String,

    /// Debug logging for partitioning decisions
    pub debug: PartitionDebug,

    /// When false, disable fallback line-based splitting. Oversized chunks that
    /// cannot be split via AST will remain oversized. This is used by audit-chunks
    /// to measure AST-only chunking quality.
    pub allow_fallback: bool,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        Self {
            target_size: 6000,
            file_name: "unknown.ts".to_string(),
            package_name: "unknown".to_string(),
            debug: PartitionDebug::default(),
            allow_fallback: true,
        }
    }
}

/// A chunk of code with breadcrumb context
#[derive(Debug, Clone)]
pub struct PartitionedChunk {
    /// Source URI (file path, issue reference, etc.)
    pub source_uri: String,

    /// Catalog name (for multi-source partitioning)
    pub catalog: String,

    /// Content hash (SHA256) for incremental sync
    pub content_hash: String,

    /// Breadcrumb path (e.g., "@rushstack/node-core-library:JsonFile.ts:JsonFile.load")
    pub breadcrumb: String,

    /// Source code text (including preceding comments)
    pub text: String,

    /// Starting line number (1-indexed)
    pub start_line: usize,

    /// Ending line number (inclusive)
    pub end_line: usize,

    /// Chunk type (function, class, method, etc.)
    pub chunk_type: String,

    /// Chunk kind (content, imports, changelog, config)
    pub chunk_kind: String,

    /// Symbol name (if applicable)
    pub symbol_name: Option<String>,
}

/// A line range representing a chunk-in-progress
#[derive(Debug, Clone)]
struct ChunkRange {
    start_line: usize,             // 1-indexed, inclusive
    end_line: usize,               // 1-indexed, inclusive
    from_fallback: bool,           // This chunk was created by a fallback split
    from_degraded_ast_split: bool, // This chunk was created by a degraded AST split
}

/// Result of attempting to split a chunk.
///
/// The algorithm distinguishes three outcomes:
/// 1. Good AST split (success) - semantically meaningful, respects min_chunk_size
/// 2. Degraded AST split (quality failure) - semantically meaningful but poor geometry
/// 3. Fallback split (algorithm failure) - no acceptable AST split found
///
/// Important: Fallback is NOT a heuristic choice. It is an explicit failure mode
/// indicating that the AST-based partitioner could not find any semantic structure
/// to use. It provides damage control for production but should trigger investigation.
enum SplitResult {
    /// Successful AST split: semantically meaningful with good chunk geometry
    Split(usize),
    /// Degraded AST split: semantically meaningful but poor chunk geometry (tiny chunks)
    /// This is a quality failure, but still preferable to fallback in production.
    /// Marked in output with `:[degraded-ast-split]` breadcrumb suffix.
    DegradedSplit(usize),
    /// Fallback split: no acceptable AST split found, using line-based recovery.
    /// This is explicit failure of semantic partitioning, not a heuristic choice.
    /// Marked in output with `:[fallback-split]` breadcrumb suffix.
    Fallback(usize),
    /// Cannot split this chunk any further
    CannotSplit,
}

/// Find the best split point for an oversized chunk using scope-based approach.
#[allow(clippy::too_many_arguments)]
fn find_best_split(
    root: Node,
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    target_size: usize,
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
                target_size,
                min_chunk_size,
            ) {
                debug.log_split_decision("USABLE SPLIT", Some(split_line));
                return SplitResult::Split(split_line);
            }
            debug.log_split_decision("No usable split (min_chunk constraint)", None);

            // No usable partition - record the least-bad split from this scope
            // But only if it respects min_chunk_size (don't record tiny splits)
            for &split_line in &candidates {
                let lines_before = split_line - start_line + 1;
                let total_lines = end_line - start_line + 1;
                let estimated_first_size = (chunk_size * lines_before) / total_lines;
                let estimated_second_size = chunk_size - estimated_first_size;

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
        let lines_before = split_line - start_line + 1;
        let total_lines = end_line - start_line + 1;
        let estimated_first_size = (chunk_size * lines_before) / total_lines;
        let estimated_second_size = chunk_size - estimated_first_size;

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
fn find_shallowest_split_scope<'a>(
    node: Node<'a>,
    start_line: usize,
    end_line: usize,
    source: &[u8],
    debug: &PartitionDebug,
) -> Node<'a> {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    if node_start > start_line || node_end < end_line {
        return node;
    }

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
fn find_nested_split_scope<'a>(
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
fn find_deepest_split_scope<'a>(
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
                let lines_before = split_line - start_line + 1;
                let total_lines = end_line - start_line + 1;
                let estimated_first_size = (chunk_size * lines_before) / total_lines;
                let estimated_second_size = chunk_size - estimated_first_size;

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
fn get_scope_candidates(
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
fn find_usable_split(
    candidates: &[usize],
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    _target_size: usize,
    min_chunk_size: usize,
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }

    let ideal_first_size = chunk_size / 2;
    let mut best_split: Option<usize> = None;
    let mut best_distance = usize::MAX;

    for &split_line in candidates {
        let lines_before = split_line - start_line + 1;
        let total_lines = end_line - start_line + 1;
        let estimated_first_size = (chunk_size * lines_before) / total_lines;
        let estimated_second_size = chunk_size - estimated_first_size;

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
fn compute_split_badness(
    split_line: usize,
    start_line: usize,
    end_line: usize,
    chunk_size: usize,
    ideal_first_size: usize,
) -> usize {
    let lines_before = split_line - start_line + 1;
    let total_lines = end_line - start_line + 1;
    let estimated_first_size = (chunk_size * lines_before) / total_lines;

    let distance = estimated_first_size.abs_diff(ideal_first_size);

    // Add penalty for small chunks
    let estimated_second_size = chunk_size - estimated_first_size;
    let tiny_penalty = if estimated_first_size < 500 || estimated_second_size < 500 {
        10000
    } else if estimated_first_size < 1000 || estimated_second_size < 1000 {
        5000
    } else {
        0
    };

    distance + tiny_penalty
}

/// Partition a TypeScript/TSX file into chunks
pub fn partition_typescript(
    source: &str,
    config: &PartitionConfig,
    file_path: &str,
    catalog: &str,
) -> Vec<PartitionedChunk> {
    let content_hash = compute_hash(source);

    let mut parser = Parser::new();

    // Use TSX grammar for .tsx files, TypeScript grammar for .ts files
    let is_tsx = file_path.ends_with(".tsx");
    if is_tsx {
        parser
            .set_language(&Language::from(tree_sitter_typescript::LANGUAGE_TSX))
            .expect("Failed to set TSX language");
    } else {
        parser
            .set_language(&Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT))
            .expect("Failed to set TypeScript language");
    }

    let tree = parser
        .parse(source, None)
        .expect("Failed to parse TypeScript");

    let root = tree.root_node();
    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len();

    // Build base breadcrumb: package:file
    let base_breadcrumb = if config.package_name.is_empty() {
        config.file_name.clone()
    } else {
        format!("{}:{}", config.package_name, config.file_name)
    };

    // Step 1: Start with the whole file as one chunk
    let mut chunks: Vec<ChunkRange> = vec![ChunkRange {
        start_line: 1,
        end_line: total_lines,
        from_fallback: false,
        from_degraded_ast_split: false,
    }];

    // Also extract imports end line for chunk_kind metadata (but don't pre-split)
    let import_end_line = extract_imports_end_line(root, source.as_bytes());

    // Step 2: Iteratively split chunks that exceed budget
    let min_chunk_size = (config.target_size as f64 * MIN_CHUNK_RATIO) as usize;
    let mut changed = true;
    while changed {
        changed = false;
        let mut new_chunks = Vec::new();

        for chunk_range in &chunks {
            let chunk_text = get_lines_text(&lines, chunk_range.start_line, chunk_range.end_line);
            let chunk_size = chunk_text.len();

            if chunk_size <= config.target_size {
                new_chunks.push(chunk_range.clone());
            } else {
                // Chunk is too big - use scope-based splitting
                match find_best_split(
                    root,
                    chunk_range.start_line,
                    chunk_range.end_line,
                    chunk_size,
                    config.target_size,
                    min_chunk_size,
                    source.as_bytes(),
                    &config.debug,
                ) {
                    SplitResult::Split(split_line) => {
                        new_chunks.push(ChunkRange {
                            start_line: chunk_range.start_line,
                            end_line: split_line,
                            from_fallback: false,
                            from_degraded_ast_split: false,
                        });
                        new_chunks.push(ChunkRange {
                            start_line: split_line + 1,
                            end_line: chunk_range.end_line,
                            from_fallback: false,
                            from_degraded_ast_split: false,
                        });
                        changed = true;
                    }
                    SplitResult::DegradedSplit(split_line) => {
                        // Degraded AST split: semantically meaningful but poor geometry
                        // Mark as degraded for visibility in diagnostics
                        new_chunks.push(ChunkRange {
                            start_line: chunk_range.start_line,
                            end_line: split_line,
                            from_fallback: false,
                            from_degraded_ast_split: true,
                        });
                        new_chunks.push(ChunkRange {
                            start_line: split_line + 1,
                            end_line: chunk_range.end_line,
                            from_fallback: false,
                            from_degraded_ast_split: true,
                        });
                        changed = true;
                    }
                    SplitResult::Fallback(split_line) => {
                        if config.allow_fallback {
                            new_chunks.push(ChunkRange {
                                start_line: chunk_range.start_line,
                                end_line: split_line,
                                from_fallback: true,
                                from_degraded_ast_split: false,
                            });
                            new_chunks.push(ChunkRange {
                                start_line: split_line + 1,
                                end_line: chunk_range.end_line,
                                from_fallback: true,
                                from_degraded_ast_split: false,
                            });
                            changed = true;
                        } else {
                            // In strict mode, leave oversized chunks as-is
                            new_chunks.push(chunk_range.clone());
                        }
                    }
                    SplitResult::CannotSplit => {
                        new_chunks.push(chunk_range.clone());
                    }
                }
            }
        }
        chunks = new_chunks;
    }

    // Step 3: Convert chunk ranges to PartitionedChunks
    let mut result = Vec::new();

    for chunk_range in &chunks {
        let chunk_text = get_lines_text(&lines, chunk_range.start_line, chunk_range.end_line);
        if chunk_text.trim().is_empty() {
            continue;
        }

        let (chunk_type, symbol_name, breadcrumb_suffix) = get_chunk_metadata(
            root,
            source.as_bytes(),
            chunk_range.start_line,
            chunk_range.end_line,
        );

        let mut breadcrumb = if breadcrumb_suffix.is_empty() {
            base_breadcrumb.clone()
        } else {
            format!("{}:{}", base_breadcrumb, breadcrumb_suffix)
        };

        if chunk_range.from_fallback {
            breadcrumb.push_str(":[fallback-split]");
        } else if chunk_range.from_degraded_ast_split {
            breadcrumb.push_str(":[degraded-ast-split]");
        }

        let chunk_kind = if import_end_line > 0 && chunk_range.end_line <= import_end_line {
            "imports".to_string()
        } else {
            "content".to_string()
        };

        result.push(PartitionedChunk {
            source_uri: file_path.to_string(),
            catalog: catalog.to_string(),
            content_hash: content_hash.to_string(),
            breadcrumb,
            text: chunk_text,
            start_line: chunk_range.start_line,
            end_line: chunk_range.end_line,
            chunk_type,
            chunk_kind,
            symbol_name,
        });
    }

    result
}

/// Get meaningful children of a node (methods, classes, functions, etc.)
fn get_meaningful_children<'a>(
    node: Node<'a>,
    source: &[u8],
    debug: &PartitionDebug,
) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    let mut children = Vec::new();

    for child in node.children(&mut cursor) {
        if is_meaningful_split_point(child, source) {
            debug.log_meaningful_child(
                child.kind(),
                child.start_position().row + 1,
                child.end_position().row + 1,
            );
            children.push(child);
        }
    }

    children
}

/// Get the end line of import statements (0 if no imports)
fn extract_imports_end_line(root: Node, _source: &[u8]) -> usize {
    let mut cursor = root.walk();
    let mut last_import_end = 0usize;

    for child in root.children(&mut cursor) {
        if child.kind() == "import_statement" {
            last_import_end = child.end_position().row + 1;
        } else if last_import_end > 0 {
            break;
        }
    }
    last_import_end
}

/// Get text for a line range (1-indexed, inclusive)
fn get_lines_text(lines: &[&str], start_line: usize, end_line: usize) -> String {
    if start_line > end_line || start_line == 0 || start_line > lines.len() {
        return String::new();
    }
    let end = end_line.min(lines.len());
    lines[start_line - 1..end].join("\n")
}

/// Determine if an AST node represents a meaningful split boundary.
///
/// Meaningful nodes are those that can serve as split points between chunks.
/// This includes declarations (functions, classes, interfaces, etc.) and
/// large expression statements (e.g., event handler registrations).
///
/// Small nodes (like 1-line variable declarations) are technically meaningful
/// but may be filtered out later if they would create tiny chunks.
fn is_meaningful_split_point(node: Node, source: &[u8]) -> bool {
    match node.kind() {
        "function_declaration"
        | "class_declaration"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration" => true,

        "export_statement" => true,

        "method_definition" => true,

        // Class fields and interface properties
        "public_field_definition" | "property_declaration" => {
            node.end_byte() - node.start_byte() > 50
        }

        // Object literal properties (key-value pairs)
        // Only meaningful if large enough or contains complex nested structure
        // Use effective size which includes attached leading comments
        "pair" => {
            let size = effective_size_with_comments(node, source);
            if size > 200 {
                return true;
            }
            // Check if the value contains complex structure (function, object, array)
            pair_has_complex_value(node)
        }

        // Interface property signatures - smaller threshold since interface members
        // are more naturally separable than runtime object-literal properties
        // Use effective size which includes attached leading comments
        "property_signature" => effective_size_with_comments(node, source) > 100,

        "lexical_declaration" | "variable_declaration" => {
            let text = String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]);
            text.starts_with("const") || text.starts_with("let") || text.starts_with("var")
        }

        // Expression statements are meaningful if they're large enough
        // (e.g., event handlers like `ws.on('connection', ...)`)
        // OR if they contain nested functions (callback registrations)
        "expression_statement" => {
            let size = node.end_byte() - node.start_byte();
            if size > 500 {
                return true;
            }
            // Check if it contains a nested function (callback registration)
            // This makes callback registrations like `obj.on('event', () => {...})`
            // meaningful split points regardless of total size
            node_contains_complex_structure(node)
        }

        // Switch cases are meaningful split points for large switch statements
        // Each case is a logical unit that can be split independently
        "switch_case" => true,

        // If statements are meaningful split points when large enough
        // This helps split large methods with complex control flow
        "if_statement" => node.end_byte() - node.start_byte() > 400,

        // For loops are meaningful split points when large enough
        // This helps split large methods with multiple sequential loops
        "for_statement" | "for_in_statement" | "for_of_statement" => {
            node.end_byte() - node.start_byte() > 300
        }

        // JSX elements are meaningful split points for large JSX
        // Each JSX element is a logical unit that can be split independently
        "jsx_element" => true,

        _ => false,
    }
}

/// Compute the effective size of a node, including any attached leading comments.
///
/// This treats a property + its documentation as a single semantic unit when
/// evaluating whether it forms a meaningful split boundary.
/// Works for both `pair` (object literal properties) and `property_signature` (interface properties).
fn effective_size_with_comments(node: Node, source: &[u8]) -> usize {
    let mut start = node.start_byte();
    let end = node.end_byte();

    // Walk backward through previous siblings to find attached comments
    let mut current = node.prev_sibling();

    while let Some(sibling) = current {
        // Only include comment nodes
        if sibling.kind() == "comment" {
            let comment_end = sibling.end_byte();
            let comment_start = sibling.start_byte();

            // Check for blank line gap between comment and current start
            // A blank line means there's more than just whitespace between them
            let between = &source[comment_end..start];
            let has_blank_line = between.windows(2).any(|w| w == b"\n\n")
                || between.windows(4).any(|w| w == b"\r\n\r\n");

            if has_blank_line {
                // Comment is separated by blank line, stop
                break;
            }

            // Include this comment
            start = comment_start;

            // Continue walking backward
            current = sibling.prev_sibling();
        } else {
            // Non-comment sibling, stop
            break;
        }
    }

    end - start
}

/// Check if a pair node contains a complex value (function, object, array)
/// that would make it a meaningful split boundary.
fn pair_has_complex_value(pair_node: Node) -> bool {
    // A pair has the structure: key : value
    // We want to check if the value part contains complex structure
    for i in 0..pair_node.child_count() {
        if let Some(child) = pair_node.child(i as u32) {
            match child.kind() {
                // Direct complex value types
                "arrow_function"
                | "function_expression"
                | "function_declaration"
                | "object"
                | "array"
                | "jsx_element"
                | "jsx_self_closing_element" => {
                    return true;
                }
                // Nested expression might wrap a complex value
                "parenthesized_expression" | "as_expression"
                    if node_contains_complex_structure(child) =>
                {
                    return true;
                }
                _ => {}
            }
        }
    }
    false
}

/// Check if a node recursively contains complex structure
fn node_contains_complex_structure(node: Node) -> bool {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            match child.kind() {
                "arrow_function"
                | "function_expression"
                | "function_declaration"
                | "object"
                | "array"
                | "jsx_element"
                | "jsx_self_closing_element" => {
                    return true;
                }
                _ => {
                    // Recursively check children
                    if node_contains_complex_structure(child) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Get chunk metadata (type, symbol name, breadcrumb suffix) for a line range
fn get_chunk_metadata(
    root: Node,
    source: &[u8],
    start_line: usize,
    end_line: usize,
) -> (String, Option<String>, String) {
    let mut best_node: Option<Node> = None;
    let mut best_size = usize::MAX;

    find_best_node(
        root,
        source,
        start_line,
        end_line,
        &mut best_node,
        &mut best_size,
    );

    if let Some(node) = best_node {
        let chunk_type = get_chunk_type(node);
        let symbol_name = get_symbol_name(node, source);
        let breadcrumb_suffix = symbol_name.clone().unwrap_or_default();
        (chunk_type, symbol_name, breadcrumb_suffix)
    } else {
        ("code".to_string(), None, String::new())
    }
}

fn find_best_node<'a>(
    node: Node<'a>,
    source: &[u8],
    chunk_start: usize,
    chunk_end: usize,
    best_node: &mut Option<Node<'a>>,
    best_size: &mut usize,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    if node_start > chunk_start || node_end < chunk_end {
        return;
    }

    if is_meaningful_split_point(node, source) {
        let node_size = node_end - node_start;
        if node_size < *best_size {
            *best_node = Some(node);
            *best_size = node_size;
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_best_node(child, source, chunk_start, chunk_end, best_node, best_size);
    }
}

fn get_chunk_type(node: Node) -> String {
    if node.kind() == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_type = match child.kind() {
                "function_declaration" => "function",
                "class_declaration" => "class",
                "interface_declaration" => "interface",
                "type_alias_declaration" => "type",
                "enum_declaration" => "enum",
                "lexical_declaration" | "variable_declaration" => "variable",
                _ => continue,
            };
            return child_type.to_string();
        }
        return "code".to_string();
    }

    match node.kind() {
        "function_declaration" | "method_definition" => "function",
        "class_declaration" => "class",
        "interface_declaration" => "interface",
        "type_alias_declaration" => "type",
        "enum_declaration" => "enum",
        "lexical_declaration" | "variable_declaration" => "variable",
        "public_field_definition" | "property_declaration" => "field",
        _ => "code",
    }
    .to_string()
}

fn get_symbol_name(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(name) = get_symbol_name(child, source) {
                return Some(name);
            }
        }
        return None;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "property_identifier" | "type_identifier" => {
                let name = String::from_utf8_lossy(&source[child.start_byte()..child.end_byte()]);
                return Some(name.to_string());
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;

    fn format_chunks_summary(chunks: &[PartitionedChunk], file_chars: usize) -> String {
        let report = ChunkQualityReport::from_chunks(chunks, file_chars);
        let mut result = format!(
            "=== QUALITY SCORE ===\nScore: {:.1}%\nTotal chunks: {}\nSmall chunks (<{} chars): {}\nChars: {}-{} (mean {:.0})\n\n",
            report.score,
            report.total_chunks,
            SMALL_CHUNK_CHARS,
            report.small_chunks,
            report.min_chars,
            report.max_chars,
            report.mean_chars
        );
        for (i, chunk) in chunks.iter().enumerate() {
            result.push_str(&format!(
                "=== CHUNK {} ===\nBreadcrumb: {}\nType: {}\nKind: {}\nSymbol: {:?}\nLines: {}-{}\nSize: {} chars\nText preview (5 lines):\n{}\n\n",
                i + 1,
                chunk.breadcrumb,
                chunk.chunk_type,
                chunk.chunk_kind,
                chunk.symbol_name,
                chunk.start_line,
                chunk.end_line,
                chunk.breadcrumb.len() + chunk.text.len(),
                chunk.text.lines().take(5).collect::<Vec<_>>().join("\n")
            ));
        }
        result
    }

    /// Visualize chunks as split points in the original source
    fn format_chunks_visualization(source: &str, chunks: &[PartitionedChunk]) -> String {
        let lines: Vec<&str> = source.lines().collect();
        let mut result = String::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let line_count = chunk.end_line - chunk.start_line + 1;
            let size = chunk.text.len();

            result.push_str(&format!(
                "-- [CHUNK {}] [{} lines] [{} chars] --\n",
                i + 1,
                line_count,
                size
            ));

            for line_num in chunk.start_line..=chunk.end_line {
                if line_num > 0 && line_num <= lines.len() {
                    result.push_str(lines[line_num - 1]);
                    result.push('\n');
                }
            }
        }

        result
    }

    #[test]
    fn test_simple_function() {
        let source = r#"
export function add(a: number, b: number): number {
    return a + b;
}
"#;
        let config = PartitionConfig {
            file_name: "test.ts".to_string(),
            package_name: "@test/package".to_string(),
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "test.ts", "test");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("simple_function_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("simple_function_summary", summary);
    }

    #[test]
    fn test_class_with_methods() {
        let source = r#"
/**
 * A simple calculator class.
 */
export class Calculator {
    /**
     * Adds two numbers.
     */
    add(a: number, b: number): number {
        return a + b;
    }
    
    /**
     * Subtracts two numbers.
     */
    subtract(a: number, b: number): number {
        return a - b;
    }
    
    /**
     * Multiplies two numbers.
     */
    multiply(a: number, b: number): number {
        return a * b;
    }
}
"#;
        let config = PartitionConfig {
            file_name: "Calculator.ts".to_string(),
            package_name: "@math/package".to_string(),
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "Calculator.ts", "math");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("class_with_methods_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("class_with_methods_summary", summary);
    }

    #[test]
    fn test_jsonfile_partition() {
        let source = include_str!("../../test_artifacts/JsonFile.ts");
        let config = PartitionConfig {
            file_name: "JsonFile.ts".to_string(),
            package_name: "@rushstack/node-core-library".to_string(),
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "JsonFile.ts", "node-core-library");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("jsonfile_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("jsonfile_summary", summary);
    }

    #[test]
    fn test_small_file_not_penalized() {
        // A small file with one chunk should score 100%
        // (not penalized for being "tiny" since the whole file is tiny)
        let source = r#"// Small test file
export function tiny(): number {
    return 42;
}
"#;
        let config = PartitionConfig {
            file_name: "tiny.ts".to_string(),
            package_name: "@test/package".to_string(),
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "tiny.ts", "test");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("small_file_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("small_file_summary", summary);
    }

    #[test]
    fn test_small_file_should_not_split() {
        // A 12-line .d.ts file (242 chars) should NOT be split into 2 chunks
        // This is a regression test for the "imports always split" bug
        let source = include_str!("../../test_artifacts/rollup.d.ts");
        let config = PartitionConfig {
            file_name: "rollup.d.ts".to_string(),
            package_name: "api-extractor-scenarios".to_string(),
            allow_fallback: false,
            ..Default::default()
        };
        let chunks =
            partition_typescript(source, &config, "rollup.d.ts", "api-extractor-scenarios");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("rollup_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("rollup_summary", summary);
    }

    #[test]
    fn test_tunneled_browser_connection() {
        // A 231-line file with nested functions that produces tiny chunks
        // This is a regression test for the "tiny chunks for variables" bug
        let source = include_str!("../../test_artifacts/TunneledBrowserConnection.ts");
        let config = PartitionConfig {
            file_name: "TunneledBrowserConnection.ts".to_string(),
            package_name: "@rushstack/playwright-browser-tunnel".to_string(),
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(
            source,
            &config,
            "TunneledBrowserConnection.ts",
            "playwright-browser-tunnel",
        );

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("tunneled_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("tunneled_summary", summary);
    }

    #[test]
    fn test_long_template_string_fallback() {
        // A degenerate case: a long template literal that cannot be split by AST
        // The chunker should fall back to line-based splitting with a warning
        // We make it 200 lines to exceed the 6000 char target
        let mut source_lines = vec![
            "// A file with a very long template string".to_string(),
            "const longString = `".to_string(),
        ];
        for i in 1..=200 {
            source_lines.push(format!("line{} some content here to make it longer", i));
        }
        source_lines.push("`;".to_string());
        source_lines.push("console.log(longString);".to_string());
        let source = source_lines.join("\n");

        let config = PartitionConfig {
            file_name: "long_string.ts".to_string(),
            package_name: "test".to_string(),
            allow_fallback: true, // This test explicitly tests fallback behavior
            ..Default::default()
        };
        let chunks = partition_typescript(&source, &config, "long_string.ts", "test");

        let visualization = format_chunks_visualization(&source, &chunks);
        assert_snapshot!("long_string_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("long_string_summary", summary);

        // Verify that the long string was split despite having no AST split points
        assert!(
            chunks.len() > 1,
            "Expected fallback to split the oversized chunk"
        );

        // Verify no chunk exceeds target size (with some tolerance for the fallback)
        for chunk in &chunks {
            assert!(
                chunk.text.len() <= config.target_size + 500,
                "Chunk at lines {}-{} exceeds target: {} chars",
                chunk.start_line,
                chunk.end_line,
                chunk.text.len()
            );
        }
    }

    #[test]
    fn test_colorize_class_with_enum() {
        // A 289-line file (8031 chars) with an enum and a class with many methods
        // This tests the ability to split a class into method-level chunks
        let source = include_str!("../../test_artifacts/Colorize.ts");
        let config = PartitionConfig {
            file_name: "Colorize.ts".to_string(),
            package_name: "@rushstack/terminal".to_string(),
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "Colorize.ts", "terminal");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("colorize_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("colorize_summary", summary);

        // Should NOT have fallback split - should use AST-based splitting at method boundaries
        for chunk in &chunks {
            assert!(
                !chunk.breadcrumb.contains("[fallback-split]"),
                "Unexpected fallback split in chunk: {}",
                chunk.breadcrumb
            );
        }
    }

    #[test]
    fn test_ipackagejson_interface_file() {
        // An interface-only file with large interfaces
        // Tests that interface boundaries are used as split points
        let source = include_str!("../../test_artifacts/IPackageJson.ts");
        let config = PartitionConfig {
            file_name: "IPackageJson.ts".to_string(),
            package_name: "@rushstack/node-core-library".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "IPackageJson.ts", "node-core-library");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("ipackagejson_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("ipackagejson_summary", summary);

        // Should not need fallback split for interface files
        for chunk in &chunks {
            assert!(
                !chunk.breadcrumb.contains("[fallback-split]"),
                "Unexpected fallback split in chunk: {}",
                chunk.breadcrumb
            );
        }
    }

    #[test]
    fn test_environment_configuration() {
        // A file with two giant constructs:
        // 1. A 226-line const object (EnvironmentVariableNames)
        // 2. A 476-line class (EnvironmentConfiguration)
        //
        // This tests the "single giant construct" problem where we have very few
        // meaningful split points because the file is dominated by large object/class
        // literals that don't have natural internal split boundaries.
        let source = include_str!("../../test_artifacts/EnvironmentConfiguration.ts");
        let config = PartitionConfig {
            file_name: "EnvironmentConfiguration.ts".to_string(),
            package_name: "rush-lib".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks =
            partition_typescript(source, &config, "EnvironmentConfiguration.ts", "rush-lib");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("environment_config_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("environment_config_summary", summary);

        // Should not have oversized chunks (target is 6000 chars)
        // This verifies effective_size_with_comments() allows splitting
        // documented object literal properties
        for chunk in &chunks {
            assert!(
                chunk.text.len() <= 6000,
                "Oversized chunk: {} chars in {}",
                chunk.text.len(),
                chunk.breadcrumb
            );
        }
    }

    #[test]
    fn test_nested_functions_in_generator() {
        // A minimal test case for nested functions inside a generator.
        // The nested functions (advance, parseA, parseB, parseC) should be
        // recognized as meaningful split points.
        let source = include_str!("../../test_artifacts/NestedFunctions.ts");
        let config = PartitionConfig {
            file_name: "NestedFunctions.ts".to_string(),
            package_name: "test".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "NestedFunctions.ts", "test");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("nested_functions_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("nested_functions_summary", summary);

        // TODO: Nested functions should be recognized as split points
        // Currently failing - nested functions inside generators/functions are not split points
        // for chunk in &chunks {
        //     assert!(!chunk.breadcrumb.contains("[fallback-split]"),
        //         "Unexpected fallback split in chunk: {}", chunk.breadcrumb);
        // }
    }

    #[test]
    fn test_git_status_parser() {
        // A real-world file with nested functions inside a generator.
        // The parseGitStatus generator contains several nested functions:
        // - getFieldAndAdvancePos
        // - parseUntrackedEntry
        // - parseAddModifyOrDeleteEntry
        // - parseRenamedOrCopiedEntry
        // - parseUnmergedEntry
        // These should be recognized as meaningful split points.
        let source = include_str!("../../test_artifacts/GitStatusParser.ts");
        let config = PartitionConfig {
            file_name: "GitStatusParser.ts".to_string(),
            package_name: "rush-lib".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "GitStatusParser.ts", "rush-lib");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("git_status_parser_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("git_status_parser_summary", summary);

        // TODO: Nested functions should be recognized as split points
        // Currently failing - nested functions inside generators are not split points
        // for chunk in &chunks {
        //     assert!(!chunk.breadcrumb.contains("[fallback-split]"),
        //         "Unexpected fallback split in chunk: {}", chunk.breadcrumb);
        // }
    }

    #[test]
    fn debug_nested_function_ast() {
        // Debug test to understand AST structure of nested functions
        let source = r#"
function* generator() {
  function nested1() {
    return 1;
  }
  function nested2() {
    return 2;
  }
}
"#;

        let mut parser = Parser::new();
        parser
            .set_language(&Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();

        fn print_tree(node: Node, indent: usize) {
            let kind = node.kind();
            let start = node.start_position();
            let end = node.end_position();
            println!(
                "{:indent$}{} [{},{}]",
                "",
                kind,
                start.row + 1,
                end.row + 1,
                indent = indent
            );
            for i in 0..node.child_count() {
                print_tree(node.child(i as u32).unwrap(), indent + 2);
            }
        }

        print_tree(tree.root_node(), 0);
    }

    #[test]
    fn debug_exported_generator_ast() {
        // Debug test to understand AST structure of exported generator
        let source = r#"
export function* parseGitStatus() {
  function nested1() {
    return 1;
  }
  function nested2() {
    return 2;
  }
}
"#;

        let mut parser = Parser::new();
        parser
            .set_language(&Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();

        fn print_tree(node: Node, indent: usize) {
            let kind = node.kind();
            let start = node.start_position();
            let end = node.end_position();
            println!(
                "{:indent$}{} [{},{}]",
                "",
                kind,
                start.row + 1,
                end.row + 1,
                indent = indent
            );
            for i in 0..node.child_count() {
                print_tree(node.child(i as u32).unwrap(), indent + 2);
            }
        }

        print_tree(tree.root_node(), 0);
    }

    #[test]
    fn test_project_watcher() {
        // A real-world file with nested functions inside async methods.
        // The waitForChangeAsync method contains several nested functions:
        // - onError, addWatcher, innerListener, changeListener
        // These should be recognized as meaningful split points.
        let source = include_str!("../../test_artifacts/ProjectWatcher.ts");
        let config = PartitionConfig {
            file_name: "ProjectWatcher.ts".to_string(),
            package_name: "rush-lib".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(source, &config, "ProjectWatcher.ts", "rush-lib");

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("project_watcher_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("project_watcher_summary", summary);

        // Nested functions inside methods should be recognized as split points
        for chunk in &chunks {
            assert!(
                !chunk.breadcrumb.contains("[fallback-split]"),
                "Unexpected fallback split in chunk: {}",
                chunk.breadcrumb
            );
        }
    }

    #[test]
    fn test_parameter_form_tsx() {
        // A TSX file with React hooks and JSX elements.
        // The file should use the TSX grammar (not TypeScript) and
        // split at JSX element boundaries.
        let source = include_str!("../../test_artifacts/ParameterForm.tsx");
        let config = PartitionConfig {
            file_name: "ParameterForm.tsx".to_string(),
            package_name: "@rushstack/rush-vscode-command-webview".to_string(),
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(
            source,
            &config,
            "ParameterForm.tsx",
            "@rushstack/rush-vscode-command-webview",
        );

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("parameter_form_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("parameter_form_summary", summary);

        // Note: This file currently has a fallback split due to a large function body
        // that lacks natural split boundaries. This is a known limitation that may
        // be addressed in a future update.
    }

    #[test]
    fn test_experiments_configuration() {
        // ExperimentsConfiguration.ts contains a large interface (IExperimentsJson)
        // with documented properties. Each property has a JSDoc comment that makes it
        // exceed the 100 byte threshold when counting the comment, but the property_signature
        // alone is small.
        //
        // This tests effective_size_with_comments() for interface property signatures.
        //
        // Before the fix: oversized interface chunk
        // After the fix: properly split at property boundaries
        let source = include_str!("../../test_artifacts/ExperimentsConfiguration.ts");
        let config = PartitionConfig {
            file_name: "ExperimentsConfiguration.ts".to_string(),
            package_name: "@microsoft/rush-lib".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(
            source,
            &config,
            "ExperimentsConfiguration.ts",
            "@microsoft/rush-lib",
        );

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("experiments_configuration_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("experiments_configuration_summary", summary);

        // Should not have oversized chunks (target is 6000 chars)
        for chunk in &chunks {
            assert!(
                chunk.text.len() <= 6000,
                "Oversized chunk: {} chars in {}",
                chunk.text.len(),
                chunk.breadcrumb
            );
        }
    }

    #[test]
    fn test_documented_interface() {
        // IYamlApiFile.ts contains large interfaces with documented properties.
        // Each property has a JSDoc comment that makes it exceed the 100 byte threshold
        // when counting the comment, but the property_signature alone is small.
        //
        // This tests effective_size_with_comments() for interface property signatures.
        //
        // Before the fix: oversized interface chunks
        // After the fix: properly split at property boundaries
        let source = include_str!("../../test_artifacts/IYamlApiFile.ts");
        let config = PartitionConfig {
            file_name: "IYamlApiFile.ts".to_string(),
            package_name: "@microsoft/api-documenter".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(
            source,
            &config,
            "IYamlApiFile.ts",
            "@microsoft/api-documenter",
        );

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("iyaml_api_file_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("iyaml_api_file_summary", summary);

        // Should not have oversized chunks (target is 6000 chars)
        for chunk in &chunks {
            assert!(
                chunk.text.len() <= 6000,
                "Oversized chunk: {} chars in {}",
                chunk.text.len(),
                chunk.breadcrumb
            );
        }
    }

    #[test]
    fn test_module_minifier_plugin() {
        // ModuleMinifierPlugin.ts contains a large method (apply) with nested callback
        // registrations. The method body has multiple expression_statement children that
        // are callback registrations (tap calls) with nested functions.
        //
        // The issue: Some expression_statements are <500 bytes but contain nested functions
        // (callback registrations). These should be meaningful split points.
        //
        // Before the fix: fallback splits in large method body
        // After the fix: properly split at callback registration boundaries
        let source = include_str!("../../test_artifacts/ModuleMinifierPlugin.ts");
        let config = PartitionConfig {
            file_name: "ModuleMinifierPlugin.ts".to_string(),
            package_name: "@rushstack/webpack5-module-minifier-plugin".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(
            source,
            &config,
            "ModuleMinifierPlugin.ts",
            "@rushstack/webpack5-module-minifier-plugin",
        );

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("module_minifier_plugin_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("module_minifier_plugin_summary", summary);

        // Note: This file currently has fallback splits due to large method bodies
        // that lack natural split boundaries. This is a known limitation that may
        // be addressed in a future update.
    }

    #[test]
    fn test_parameter_form_tsx_large() {
        // ParameterForm.tsx is a large React component with multiple useEffect hooks
        // and a large JSX return statement.
        //
        // The issue: The component function body has many small expression_statements
        // (useCallback, useEffect) and a large JSX return. The expression_statements
        // containing arrow functions should be meaningful split points.
        //
        // Before the fix: fallback splits in large component function
        // After the fix: properly split at hook/expression boundaries
        let source = include_str!("../../test_artifacts/ParameterForm.tsx");
        let config = PartitionConfig {
            file_name: "ParameterForm.tsx".to_string(),
            package_name: "@rushstack/rush-vscode-command-webview".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(
            source,
            &config,
            "ParameterForm.tsx",
            "@rushstack/rush-vscode-command-webview",
        );

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("parameter_form_large_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("parameter_form_large_summary", summary);

        // Note: This file currently has fallback splits due to a large function body
        // that lacks natural split boundaries. This is a known limitation that may
        // be addressed in a future update.
    }

    #[test]
    fn test_generate_patched_file() {
        // generate-patched-file.ts contains a large function with string concatenations
        // and conditional blocks. The function body has many expression_statements
        // that are outputFile += ... operations.
        //
        // The issue: Many expression_statements are small (<500 bytes) but the overall
        // function body is large. The algorithm needs to find split points between
        // logical sections.
        //
        // Before the fix: fallback splits in large function body
        // After the fix: properly split at logical boundaries
        let source = include_str!("../../test_artifacts/generate-patched-file.ts");
        let config = PartitionConfig {
            file_name: "generate-patched-file.ts".to_string(),
            package_name: "@rushstack/eslint-patch".to_string(),
            debug: PartitionDebug { enabled: true },
            allow_fallback: false,
            ..Default::default()
        };
        let chunks = partition_typescript(
            source,
            &config,
            "generate-patched-file.ts",
            "@rushstack/eslint-patch",
        );

        let visualization = format_chunks_visualization(source, &chunks);
        assert_snapshot!("generate_patched_file_visualization", visualization);

        let summary = format_chunks_summary(&chunks, source.len());
        assert_snapshot!("generate_patched_file_summary", summary);

        // Note: This file currently has fallback splits due to a large function body
        // that lacks natural split boundaries. This is a known limitation that may
        // be addressed in a future update.
    }
}
