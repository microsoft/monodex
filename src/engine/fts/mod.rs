//! Full-text search via Tantivy.
//!
//! Purpose: Export FTS submodules and public surface for the rest of the codebase.
//! Edit here when: Adding or removing FTS submodules, or changing the public surface
//! re-exported from this folder.
//! Do not edit here for: FTS indexing logic (see `indexing.rs`), tokenizer behavior
//! (see `tokenizer.rs`), schema (see `schema.rs`), search semantics (see `search.rs`),
//! manifest handling (see `manifest.rs`), index management (see `index.rs`),
//! error types (see `error.rs`), or vector search (see `engine/storage/chunks/`).

mod error;
mod index;
mod indexing;
mod manifest;
mod schema;
mod search;
mod tokenizer;

#[cfg(test)]
mod tests;

pub use index::{FTS_HEAP_BUDGET_BYTES, FtsIndex, FtsOpenExistingOutcome, FtsStaleReason};
pub use indexing::{FtsIndexingStats, index_chunks_for_fts};
pub use manifest::{FtsManifest, ManifestRead};
pub use schema::{FtsSchemaFields, fts_schema, get_fts_fields};
pub use search::{FtsHit, FtsSearchOutcome, fts_search};
pub use tokenizer::{FTS_TOKENIZER_NAME, MonodexFtsTokenizer, tokenize_text};
