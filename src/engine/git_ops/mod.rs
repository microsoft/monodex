//! Purpose: Git-aware enumeration and blob reading for crawl sources.
//! Edit here when: Adding or renaming git_ops submodules, or changing the public surface re-exported from this folder.
//! Do not edit here for: the `BlobSource` trait or `FileEntry` (see `blob_source.rs`), package-index lookup and extraction (see `package_index.rs`), gix-based commit traversal (see `commit.rs`), subprocess-based working-tree reading (see `working_dir.rs`).

mod commit;
mod working_dir;

mod blob_source;
mod package_index;
#[cfg(test)]
mod tests;

pub use blob_source::{BlobSource, CommitBlobSource, FileEntry, WorkingDirBlobSource};
pub use package_index::{PackageIndex, extract_package_name_from_bytes};

// Re-export items used by app/ (via engine/mod.rs facade)
pub use self::commit::resolve_commit_oid;
