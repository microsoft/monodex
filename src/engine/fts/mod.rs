//! Full-text search via Tantivy.
//!
//! Purpose: Implement FTS indexing and search for code chunks using Tantivy.
//! Edit here when: Changing FTS indexing logic, tokenizer behavior, schema, or search semantics.
//! Do not edit here for: Vector search (see `engine/storage/chunks/`), CLI handlers (see `app/commands/`).

pub mod index;
pub mod indexing;
pub mod manifest;
pub mod schema;
pub mod search;
pub mod tokenizer;

#[cfg(test)]
mod tests;

pub use index::{FTS_HEAP_BUDGET_BYTES, FtsIndex};
pub use indexing::index_chunks_for_fts;
pub use manifest::{FtsManifest, ManifestRead};
pub use schema::{FtsSchemaFields, fts_schema, get_fts_fields};
pub use search::{FtsHit, FtsSearchOutcome, fts_search};
pub use tokenizer::{FTS_TOKENIZER_NAME, MonodexFtsTokenizer, tokenize_text};
