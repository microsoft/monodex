//! Purpose: Partition-based code chunking for TypeScript/TSX files.
//! Edit here when: Changing the public API of the partitioner module, or adding new submodules.
//! Do not edit here for: Algorithm changes — edit split_search.rs, node_analysis.rs, or scoring.rs.
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

mod debug;
mod node_analysis;
mod partition;
mod scoring;
mod split_search;
mod types;

pub use debug::PartitionDebug;
pub use partition::partition_typescript;
pub use scoring::{ChunkQualityReport, chunk_quality_score};
pub use types::{
    PartitionConfig, PartitionError, PartitionedChunk, SMALL_CHUNK_CHARS, TARGET_CHARS,
};

#[cfg(test)]
mod tests;
