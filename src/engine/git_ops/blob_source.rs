//! Purpose: the `BlobSource` abstraction over commit and working-directory crawl sources, and the `FileEntry` vocabulary.
//! Edit here when: Changing the `BlobSource` trait, `FileEntry`, or the two `BlobSource` impls.
//! Do not edit here for: gix-based commit traversal (see `commit.rs`), subprocess-based working-tree reading (see `working_dir.rs`), package index (see `package_index.rs`).

use anyhow::Result;

// Re-export public API from submodules
pub use super::commit::{build_package_index_for_commit, enumerate_commit_tree, read_blob_content};
use super::package_index::PackageIndex;
pub use super::working_dir::{
    build_package_index_for_working_dir, enumerate_working_directory, read_working_file_content,
};

/// A file entry with its relative path and Git blob ID.
///
/// This struct is used by both commit-based and working-directory enumeration.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub relative_path: String,
    pub blob_id: String,
}

/// Trait for abstracting over crawl sources (commit tree vs working directory).
///
/// This trait provides behavior-only methods for the three operations that
/// differ between commit-based and working-directory crawling:
/// - enumerating files
/// - reading file content
/// - building the package index
///
/// Source identity (`source_kind`, `commit_oid`) is NOT on the trait.
/// Those values travel as `CrawlSourceMetadata` alongside the trait object,
/// keeping the trait focused on behavior and preventing mode-branching patterns.
pub trait BlobSource {
    /// Enumerate all files in this source.
    fn enumerate(&self) -> Result<Vec<FileEntry>>;

    /// Read the content of a file from this source.
    fn read_content(&self, file: &FileEntry) -> Result<Vec<u8>>;

    /// Build the package index for this source.
    fn build_package_index(&self) -> Result<PackageIndex>;
}

/// Blob source for a specific Git commit.
///
/// Reads file content and blob IDs directly from Git objects,
/// ensuring deterministic, reproducible indexing.
pub struct CommitBlobSource {
    repo: gix::Repository,
    commit_oid: String,
}

impl CommitBlobSource {
    pub fn new(repo_path: &std::path::Path, commit_oid: String) -> Result<Self> {
        let repo = gix::open(repo_path)
            .map_err(|e| anyhow::anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;
        Ok(Self { repo, commit_oid })
    }
}

impl BlobSource for CommitBlobSource {
    fn enumerate(&self) -> Result<Vec<FileEntry>> {
        enumerate_commit_tree(&self.repo, &self.commit_oid)
    }

    fn read_content(&self, file: &FileEntry) -> Result<Vec<u8>> {
        read_blob_content(&self.repo, &file.blob_id)
    }

    fn build_package_index(&self) -> Result<PackageIndex> {
        build_package_index_for_commit(&self.repo, &self.commit_oid)
    }
}

/// Blob source for the working directory.
///
/// Reads files directly from the filesystem and computes Git-compatible
/// blob IDs that respect .gitattributes and clean filters.
pub struct WorkingDirBlobSource {
    repo_path: std::path::PathBuf,
}

impl WorkingDirBlobSource {
    pub fn new(repo_path: std::path::PathBuf) -> Self {
        Self { repo_path }
    }
}

impl BlobSource for WorkingDirBlobSource {
    fn enumerate(&self) -> Result<Vec<FileEntry>> {
        enumerate_working_directory(&self.repo_path)
    }

    fn read_content(&self, file: &FileEntry) -> Result<Vec<u8>> {
        read_working_file_content(&self.repo_path, &file.relative_path)
    }

    fn build_package_index(&self) -> Result<PackageIndex> {
        build_package_index_for_working_dir(&self.repo_path)
    }
}
