//! Chunk table operations facade.
//!
//! Purpose: Re-export chunk storage types and operations from the chunks submodule.
//!
//! Edit here when: Adding or removing chunks submodules, or changing the public
//!   surface re-exported from this directory.
//! Do not edit here for: Chunk storage operations (see storage.rs), Arrow encoding
//!   and decoding for chunk rows (see arrow_encoding.rs).

mod arrow_encoding;
mod storage;

#[cfg(test)]
mod tests;

pub use storage::{ChunkStorage, SentinelStatus, StorageProgressEvent};
