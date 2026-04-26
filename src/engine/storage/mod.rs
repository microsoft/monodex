//! LanceDB storage layer for monodex.
//!
//! Purpose: Provide a clean typed API for reading/writing chunks and label metadata.
//! This is the narrow seam between the application and LanceDB.
//!
//! Edit here when: Adding new storage operations, changing LanceDB table schemas,
//!   or modifying the database open/validate logic.
//! Do not edit here for: Chunking logic (see engine/partitioner/), CLI handlers (see app/commands/).

mod chunks;
mod database;
mod labels;
mod rows;

pub use chunks::ChunkStorage;
pub use database::{Database, META_FILE, MetaFile, err_schema_mismatch};
pub use labels::LabelStorage;
pub use rows::{ChunkRow, LabelMetadataRow, ScoredChunkRow};

/// LanceDB crate version. Keep in sync with Cargo.toml `lancedb` dependency.
pub const LANCEDB_CRATE_VERSION: &str = "0.27";
