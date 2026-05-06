//! Full-text search via Tantivy.
//!
//! Purpose: Implement FTS indexing and search for code chunks using Tantivy.
//! Edit here when: Changing FTS indexing logic, tokenizer behavior, schema, or search semantics.
//! Do not edit here for: Vector search (see `engine/storage/chunks/`), CLI handlers (see `app/commands/`).

pub mod tokenizer;

// These modules will be filled in Stage 4:
// pub mod schema;
// pub mod manifest;
// pub mod index;
// pub mod indexing;
// pub mod search;

pub use tokenizer::FTS_TOKENIZER_NAME;
