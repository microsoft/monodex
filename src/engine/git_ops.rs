//! Purpose: Git operations for commit-based and working-directory crawling.
//! Edit here when: Changing how files are enumerated from Git trees, blob ID computation, or package index building.
//! Do not edit here for: Chunking logic (use chunker.rs), storage operations (use storage/), crawl config (use crawl_config.rs).
//!
//! This module defines the `BlobSource` trait for abstracting over commit-based
//! and working-directory crawl sources. The trait provides behavior-only methods
//! (enumerate files, read content, build package index) without exposing source
//! identity, which travels separately as `CrawlSourceMetadata`.

use anyhow::{Result, anyhow};
use gix::ObjectId;
use gix::objs::TreeRefIter;
use gix::traverse::tree::Recorder;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Minimum required Git version for working directory crawling.
/// Git 2.35.0 introduced `git ls-files --format` which we use for blob ID extraction.
const MIN_GIT_VERSION: &str = "2.35.0";

/// Check if the installed Git version meets the minimum requirement.
fn ensure_git_version() -> Result<()> {
    let output = Command::new("git")
        .arg("--version")
        .output()
        .map_err(|e| anyhow!("Failed to run 'git --version': {}", e))?;

    if !output.status.success() {
        return Err(anyhow!("'git --version' failed"));
    }

    let version_str = String::from_utf8_lossy(&output.stdout);
    // Parse "git version X.Y.Z" format
    let version = version_str
        .trim()
        .strip_prefix("git version ")
        .ok_or_else(|| anyhow!("Unexpected git version format: {}", version_str.trim()))?;

    if !version_at_least(version, MIN_GIT_VERSION) {
        return Err(anyhow!(
            "Git version {} is required, but found {}",
            MIN_GIT_VERSION,
            version
        ));
    }

    Ok(())
}

/// Compare two semver-like version strings.
fn version_at_least(actual: &str, required: &str) -> bool {
    let actual_parts: Vec<u32> = actual.split('.').filter_map(|s| s.parse().ok()).collect();
    let required_parts: Vec<u32> = required.split('.').filter_map(|s| s.parse().ok()).collect();

    for i in 0..required_parts.len().max(actual_parts.len()) {
        let actual_val = actual_parts.get(i).copied().unwrap_or(0);
        let required_val = required_parts.get(i).copied().unwrap_or(0);
        if actual_val > required_val {
            return true;
        }
        if actual_val < required_val {
            return false;
        }
    }
    true
}

/// A file entry with its relative path and Git blob ID.
///
/// This unified struct is used by both commit-based and working-directory
/// enumeration, replacing the former separate `FileEntry` and `WorkingDirEntry`.
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
    repo_path: std::path::PathBuf,
    commit_oid: String,
}

impl CommitBlobSource {
    pub fn new(repo_path: std::path::PathBuf, commit_oid: String) -> Self {
        Self {
            repo_path,
            commit_oid,
        }
    }
}

impl BlobSource for CommitBlobSource {
    fn enumerate(&self) -> Result<Vec<FileEntry>> {
        enumerate_commit_tree(&self.repo_path, &self.commit_oid)
    }

    fn read_content(&self, file: &FileEntry) -> Result<Vec<u8>> {
        read_blob_content(&self.repo_path, &file.blob_id)
    }

    fn build_package_index(&self) -> Result<PackageIndex> {
        build_package_index_for_commit(&self.repo_path, &self.commit_oid)
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

pub fn resolve_commit_oid(repo_path: &Path, commit: &str) -> Result<String> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    Ok(commit_id.to_hex().to_string())
}

pub struct PackageIndex {
    package_name_by_dir: HashMap<String, String>,
}

impl PackageIndex {
    pub fn new() -> Self {
        Self {
            package_name_by_dir: HashMap::new(),
        }
    }

    pub fn find_package_name(&self, relative_path: &str) -> Option<&str> {
        let path = Path::new(relative_path);
        let mut current = path.parent().unwrap_or(path);

        loop {
            let dir_str = current.to_string_lossy();
            let dir_key = if dir_str == "." {
                String::new()
            } else {
                dir_str.replace('\\', "/")
            };

            if let Some(name) = self.package_name_by_dir.get(&dir_key) {
                return Some(name);
            }

            if current == Path::new("") || current == Path::new(".") {
                if let Some(name) = self.package_name_by_dir.get("") {
                    return Some(name);
                }
                break;
            }

            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }

        None
    }
}

impl Default for PackageIndex {
    fn default() -> Self {
        Self::new()
    }
}

pub fn enumerate_commit_tree(repo_path: &Path, commit: &str) -> Result<Vec<FileEntry>> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    let commit_obj = repo
        .find_object(commit_id)
        .map_err(|e| anyhow!("Failed to find commit object: {}", e))?;

    let tree_id: ObjectId = {
        let commit = commit_obj
            .try_into_commit()
            .map_err(|_| anyhow!("'{}' is not a commit", commit))?;
        commit
            .tree_id()
            .map_err(|e| anyhow!("Failed to get tree ID: {}", e))?
            .detach()
    };

    let tree_data = {
        let tree_obj = repo
            .find_object(tree_id)
            .map_err(|e| anyhow!("Failed to find tree object: {}", e))?;
        tree_obj.data.clone()
    };

    let mut recorder = Recorder::default();
    gix::traverse::tree::breadthfirst(
        TreeRefIter::from_bytes(&tree_data),
        &mut gix::traverse::tree::breadthfirst::State::default(),
        repo.objects,
        &mut recorder,
    )
    .map_err(|e| anyhow!("Failed to traverse tree: {}", e))?;

    Ok(recorder
        .records
        .into_iter()
        .filter(|entry| entry.mode.is_blob())
        .map(|entry| FileEntry {
            relative_path: entry.filepath.to_string(),
            blob_id: entry.oid.to_hex().to_string(),
        })
        .collect())
}

pub fn read_blob_content(repo_path: &Path, blob_id: &str) -> Result<Vec<u8>> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let object_id = ObjectId::from_hex(blob_id.as_bytes())
        .map_err(|e| anyhow!("Invalid blob ID '{}': {}", blob_id, e))?;

    let blob = repo
        .find_object(object_id)
        .map_err(|e| anyhow!("Failed to find blob '{}': {}", blob_id, e))?
        .try_into_blob()
        .map_err(|_| anyhow!("Object '{}' is not a blob", blob_id))?;

    Ok(blob.data.to_vec())
}

pub fn build_package_index_for_commit(repo_path: &Path, commit: &str) -> Result<PackageIndex> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    let commit_obj = repo
        .find_object(commit_id)
        .map_err(|e| anyhow!("Failed to find commit object: {}", e))?;

    let tree_id: ObjectId = {
        let commit = commit_obj
            .try_into_commit()
            .map_err(|_| anyhow!("'{}' is not a commit", commit))?;
        commit
            .tree_id()
            .map_err(|e| anyhow!("Failed to get tree ID: {}", e))?
            .detach()
    };

    let tree_data = {
        let tree_obj = repo
            .find_object(tree_id)
            .map_err(|e| anyhow!("Failed to find tree object: {}", e))?;
        tree_obj.data.clone()
    };

    let mut recorder = Recorder::default();
    gix::traverse::tree::breadthfirst(
        TreeRefIter::from_bytes(&tree_data),
        &mut gix::traverse::tree::breadthfirst::State::default(),
        repo.objects.clone(),
        &mut recorder,
    )
    .map_err(|e| anyhow!("Failed to traverse tree: {}", e))?;

    let package_json_entries: Vec<(String, ObjectId)> = recorder
        .records
        .iter()
        .filter(|entry| entry.mode.is_blob())
        .filter_map(|entry| {
            let filepath_bytes: &[u8] = entry.filepath.as_ref();
            let filename = filepath_bytes
                .rsplit(|b| *b == b'/')
                .next()
                .unwrap_or_default();

            if filename == b"package.json" {
                let filepath_str = String::from_utf8_lossy(filepath_bytes);
                let dir_path = filepath_str
                    .strip_suffix("/package.json")
                    .or_else(|| filepath_str.strip_suffix("package.json"))
                    .unwrap_or("")
                    .to_string();
                Some((dir_path, entry.oid))
            } else {
                None
            }
        })
        .collect();

    let mut index = PackageIndex::new();
    for (dir_path, blob_id) in package_json_entries {
        if let Ok(obj) = repo.find_object(blob_id)
            && let Ok(blob) = obj.try_into_blob()
            && let Some(name) = extract_package_name_from_bytes(&blob.data)
        {
            index.package_name_by_dir.insert(dir_path, name);
        }
    }

    Ok(index)
}

#[derive(Deserialize)]
pub struct PackageJsonName {
    name: Option<String>,
}

/// Extract the "name" field from a package.json file content.
///
/// Uses proper JSON parsing (not string search) to handle edge cases
/// like nested "name" fields in other objects.
pub fn extract_package_name_from_bytes(content: &[u8]) -> Option<String> {
    serde_json::from_slice::<PackageJsonName>(content)
        .ok()?
        .name
        .filter(|name| !name.is_empty())
}

/// Map of relative path -> Git blob ID for the working tree.
/// This correctly handles Git filter semantics (e.g., .gitattributes, EOL normalization).
#[derive(Debug, Clone, Default)]
pub struct WorkingTreeBlobMap {
    pub blobs_by_path: HashMap<String, String>,
}

/// Build a complete map of working tree file paths to their Git blob IDs.
///
/// This uses Git CLI batch commands to ensure correct blob IDs that respect
/// .gitattributes, clean filters, and other repo-specific settings.
///
/// Algorithm:
/// 1. Get all tracked files with their indexed blob IDs via `git ls-files`
/// 2. Detect dirty (modified, deleted, untracked) paths via `git status`
/// 3. Re-hash changed files via batched `git hash-object --stdin-paths`
pub fn build_working_tree_blob_map(repo_root: &Path) -> Result<WorkingTreeBlobMap> {
    // Ensure Git version supports the features we need
    ensure_git_version()?;

    // Step 1: Get tracked files with their blob IDs
    let mut tracked = git_list_tracked_blob_ids(repo_root)?;

    // Step 2: Detect dirty paths
    let dirty = git_list_dirty_paths(repo_root)?;

    // Step 3: Build list of paths to re-hash
    let mut to_hash: Vec<String> = Vec::new();
    for entry in dirty {
        if entry.exists_in_worktree {
            to_hash.push(entry.path);
        } else {
            // Deleted file - remove from tracked
            tracked.remove(&entry.path);
        }
    }

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    to_hash.retain(|p| seen.insert(p.clone()));

    // Step 4: Batch hash changed files
    if !to_hash.is_empty() {
        let hashed = git_hash_object_batch(repo_root, &to_hash)?;
        for (path, blob_id) in hashed {
            tracked.insert(path, blob_id);
        }
    }

    Ok(WorkingTreeBlobMap {
        blobs_by_path: tracked,
    })
}

/// Result from parsing `git status` output
struct DirtyEntry {
    path: String,
    exists_in_worktree: bool,
}

/// Get all tracked files with their blob IDs using `git ls-files`.
///
/// Format: `<mode> <blob_id> <stage>\t<path>`
fn git_list_tracked_blob_ids(repo_root: &Path) -> Result<HashMap<String, String>> {
    let output = Command::new("git")
        .args([
            "--no-optional-locks",
            "ls-files",
            "--cached",
            "-z",
            "--full-name",
            "--format=%(objectname)\t%(path)",
        ])
        .current_dir(repo_root)
        .output()
        .map_err(|e| anyhow!("Failed to run git ls-files: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git ls-files failed: {}", stderr));
    }

    let mut result = HashMap::new();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Output is NUL-delimited, each entry is: `<blob_id>\t<path>\0`
    for entry in stdout.split('\0') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        if let Some((blob_id, path)) = entry.split_once('\t') {
            result.insert(path.to_string(), blob_id.to_string());
        }
    }

    Ok(result)
}

/// Detect dirty (modified, deleted, untracked) paths using `git status`.
fn git_list_dirty_paths(repo_root: &Path) -> Result<Vec<DirtyEntry>> {
    let output = Command::new("git")
        .args([
            "--no-optional-locks",
            "status",
            "-z",
            "-u",
            "--no-renames",
            "--ignore-submodules",
            "--no-ahead-behind",
        ])
        .current_dir(repo_root)
        .output()
        .map_err(|e| anyhow!("Failed to run git status: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git status failed: {}", stderr));
    }

    let mut result = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse NUL-delimited status output
    // Format: XY PATH\0 or XY ORIG_PATH -> NEW_PATH\0 for renames
    // We use --no-renames so we only get XY PATH\0
    for part in stdout.split('\0') {
        if part.is_empty() {
            continue;
        }

        // Status format: XY followed by a space, then path
        // X = index status, Y = worktree status
        // We care about: .M (modified in worktree), .D (deleted), ?? (untracked)
        if part.len() < 3 {
            continue;
        }

        let xy = &part[0..2];
        let path = &part[3..]; // Skip "XY "

        match xy {
            " M" | "AM" | "MM" => {
                // Modified in worktree (not staged, or both staged and unstaged changes)
                result.push(DirtyEntry {
                    path: path.to_string(),
                    exists_in_worktree: true,
                });
            }
            " D" | "AD" | "MD" => {
                // Deleted in worktree
                result.push(DirtyEntry {
                    path: path.to_string(),
                    exists_in_worktree: false,
                });
            }
            "??" => {
                // Untracked
                result.push(DirtyEntry {
                    path: path.to_string(),
                    exists_in_worktree: true,
                });
            }
            _ => {
                // Other statuses: staged changes, etc.
                // For staged-only changes (M., A., D.), the blob ID from ls-files
                // is already correct for the staged version
                // We only re-hash worktree changes
                if xy.chars().nth(1) == Some('M') {
                    // Y = M means worktree modified
                    result.push(DirtyEntry {
                        path: path.to_string(),
                        exists_in_worktree: true,
                    });
                } else if xy.chars().nth(1) == Some('D') {
                    // Y = D means worktree deleted
                    result.push(DirtyEntry {
                        path: path.to_string(),
                        exists_in_worktree: false,
                    });
                }
            }
        }
    }

    Ok(result)
}

/// Batch hash files using `git hash-object --stdin-paths`.
///
/// Returns a list of (path, blob_id) pairs in the same order as input.
fn git_hash_object_batch(repo_root: &Path, paths: &[String]) -> Result<Vec<(String, String)>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Create stdin with all paths, one per line
    let stdin_input = paths.join("\n");

    let output = Command::new("git")
        .args(["--no-optional-locks", "hash-object", "--stdin-paths"])
        .current_dir(repo_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("Failed to spawn git hash-object: {}", e))?;

    // Write paths to stdin
    let mut child = output;
    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_input.as_bytes())
            .map_err(|e| anyhow!("Failed to write to git hash-object stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| anyhow!("Failed to wait for git hash-object: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git hash-object failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let blob_ids: Vec<&str> = stdout.lines().collect();

    // Verify we got the expected number of blob IDs
    if blob_ids.len() != paths.len() {
        return Err(anyhow!(
            "git hash-object returned {} blob IDs for {} paths",
            blob_ids.len(),
            paths.len()
        ));
    }

    Ok(paths
        .iter()
        .cloned()
        .zip(blob_ids.iter().map(|s| s.to_string()))
        .collect())
}

/// Enumerate files from the working directory using Git-aware blob IDs.
///
/// This function builds a complete blob map using Git CLI batch commands,
/// then walks the filesystem to filter by crawl config. The blob IDs
/// correctly respect .gitattributes, clean filters, and other repo-specific settings.
pub fn enumerate_working_directory(repo_path: &Path) -> Result<Vec<FileEntry>> {
    // Build the Git-aware blob map
    let blob_map = build_working_tree_blob_map(repo_path)?;

    let mut entries: Vec<FileEntry> = Vec::new();

    for entry in walkdir::WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Don't filter the root directory itself
            if e.path() == repo_path {
                return true;
            }

            let path = e.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();

            // Skip all hidden files and directories (dot-prefixed).
            if name.starts_with('.') {
                return false;
            }

            true
        })
    {
        let entry = entry.map_err(|e| anyhow!("Failed to read directory entry: {}", e))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let relative_path = path
            .strip_prefix(repo_path)
            .map_err(|e| anyhow!("Failed to strip prefix: {}", e))?
            .to_string_lossy()
            .replace('\\', "/");

        // Look up the blob ID from our Git-aware map.
        // Note: Deleted files are already removed from blob_map by build_working_tree_blob_map(),
        // and won't be found by walkdir() anyway since they don't exist on disk.
        // This ensures deleted files are never processed as candidates.
        if let Some(blob_id) = blob_map.blobs_by_path.get(&relative_path) {
            entries.push(FileEntry {
                relative_path,
                blob_id: blob_id.clone(),
            });
        }
        // Files not in blob_map are either:
        // - in .gitignore (shouldn't be indexed)
        // - deleted (already removed from blob_map)
        // We skip them silently.
    }

    Ok(entries)
}

/// Build package index from the working directory.
/// This function walks the filesystem to find all package.json files and extracts
/// their package names. All hidden directories (dot-prefixed) are excluded.
pub fn build_package_index_for_working_dir(repo_path: &Path) -> Result<PackageIndex> {
    let mut index = PackageIndex::new();

    for entry in walkdir::WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let path = e.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();

            // Skip all hidden files and directories (dot-prefixed).
            // This includes .git, .cache, .temp, .idea, .vscode, etc.
            if name.starts_with('.') {
                return false;
            }

            true
        })
    {
        let entry = entry.map_err(|e| anyhow!("Failed to read directory entry: {}", e))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        if file_name != "package.json" {
            continue;
        }

        let dir_path = path
            .parent()
            .and_then(|p| p.strip_prefix(repo_path).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        if let Ok(content) = std::fs::read(path)
            && let Some(name) = extract_package_name_from_bytes(&content)
        {
            index.package_name_by_dir.insert(dir_path, name);
        }
    }

    Ok(index)
}

pub fn read_working_file_content(repo_path: &Path, relative_path: &str) -> Result<Vec<u8>> {
    let full_path = repo_path.join(relative_path);
    std::fs::read(&full_path).map_err(|e| anyhow!("Failed to read file {}: {}", relative_path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_enumerate_commit_tree_current_repo() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entries = enumerate_commit_tree(&repo_path, "HEAD").expect("Failed to enumerate");
        assert!(!entries.is_empty(), "Should have found some files");
        assert!(entries.iter().any(|e| e.relative_path == "README.md"));
        assert!(entries.iter().any(|e| e.relative_path == "Cargo.toml"));
    }

    #[test]
    fn test_read_blob_content_current_repo() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entries = enumerate_commit_tree(&repo_path, "HEAD").expect("Failed to enumerate");
        let readme = entries
            .iter()
            .find(|e| e.relative_path == "README.md")
            .unwrap();
        let content = read_blob_content(&repo_path, &readme.blob_id).expect("Failed to read blob");
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("Monodex"));
    }

    #[test]
    fn test_build_package_index() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let _index =
            build_package_index_for_commit(&repo_path, "HEAD").expect("Failed to build index");
    }

    #[test]
    fn test_find_package_name_uses_repo_relative_paths() {
        let mut index = PackageIndex::new();
        index.package_name_by_dir.insert(
            "libraries/node-core-library".to_string(),
            "@rushstack/node-core-library".to_string(),
        );
        index
            .package_name_by_dir
            .insert("".to_string(), "root-package".to_string());

        assert_eq!(
            index.find_package_name("libraries/node-core-library/src/JsonFile.ts"),
            Some("@rushstack/node-core-library")
        );
        assert_eq!(
            index.find_package_name("libraries/node-core-library/package.json"),
            Some("@rushstack/node-core-library")
        );
        assert_eq!(index.find_package_name("src/main.rs"), Some("root-package"));
    }

    #[test]
    fn test_extract_package_name_from_bytes() {
        let json = br#"{"name": "@scope/package-name", "version": "1.0.0"}"#;
        let name = extract_package_name_from_bytes(json);
        assert_eq!(name, Some("@scope/package-name".to_string()));

        let json2 = br#"{
  "name": "simple-package",
  "version": "2.0.0"
}"#;
        let name2 = extract_package_name_from_bytes(json2);
        assert_eq!(name2, Some("simple-package".to_string()));
    }

    #[test]
    fn test_extract_package_name_from_bytes_ignores_nested_name_fields() {
        let json = br#"{
  "exports": {
    ".": {
      "name": "nested-name-should-not-win"
    }
  },
  "name": "top-level-package"
}"#;
        let name = extract_package_name_from_bytes(json);
        assert_eq!(name, Some("top-level-package".to_string()));
    }

    #[test]
    fn test_nested_package_directory_key_round_trip() {
        let relative_package_json = "libraries/node-core-library/package.json";
        let dir_path = relative_package_json
            .strip_suffix("/package.json")
            .or_else(|| relative_package_json.strip_suffix("package.json"))
            .unwrap_or("");

        assert_eq!(dir_path, "libraries/node-core-library");

        let mut index = PackageIndex::new();
        index.package_name_by_dir.insert(
            dir_path.to_string(),
            "@rushstack/node-core-library".to_string(),
        );

        assert_eq!(
            index.find_package_name("libraries/node-core-library/src/JsonFile.ts"),
            Some("@rushstack/node-core-library")
        );
    }

    #[test]
    #[ignore = "slow integration test that walks the entire repository"]
    fn test_enumerate_working_directory() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entries =
            enumerate_working_directory(&repo_path).expect("Failed to enumerate working directory");
        assert!(!entries.is_empty(), "Should have found some files");
        // README.md should be found (it's a regular file that should be found)
        // Note: Hidden files/directories (dot-prefixed) are skipped during enumeration
        assert!(entries.iter().any(|e| e.relative_path == "README.md"));
        // All entries should have a 40-character hex blob_id
        for entry in &entries {
            assert_eq!(
                entry.blob_id.len(),
                40,
                "blob_id should be 40 chars: {}",
                entry.blob_id
            );
            assert!(
                entry.blob_id.chars().all(|c| c.is_ascii_hexdigit()),
                "blob_id should be hex: {}",
                entry.blob_id
            );
        }
    }

    /// Regression test for BF.WD.1: file_id must be identical between commit and working-dir modes
    /// for unchanged files. This test creates a minimal Git repo and verifies the invariant.
    #[test]
    fn test_file_id_identical_between_modes() {
        use std::fs;
        use tempfile::TempDir;

        // Create a temporary directory
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let repo_path = temp_dir.path();

        // Initialize a minimal Git repo
        let git_init = Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .expect("Failed to run git init");
        assert!(git_init.status.success(), "git init failed");

        // Configure local user for this repo
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(repo_path)
            .output()
            .expect("Failed to set user.name");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo_path)
            .output()
            .expect("Failed to set user.email");

        // Create and commit a test file
        let test_file = repo_path.join("test.txt");
        fs::write(&test_file, "Hello, World!\n").expect("Failed to write test file");

        Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(repo_path)
            .output()
            .expect("Failed to run git add");

        let git_commit = Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(repo_path)
            .output()
            .expect("Failed to run git commit");
        assert!(git_commit.status.success(), "git commit failed");

        // Get commit-mode entries
        let commit_entries =
            enumerate_commit_tree(repo_path, "HEAD").expect("Failed to enumerate commit");
        let commit_entry = commit_entries
            .iter()
            .find(|e| e.relative_path == "test.txt")
            .expect("test.txt should exist in commit");

        // Get working-dir entries (file is unchanged)
        let workdir_entries =
            enumerate_working_directory(repo_path).expect("Failed to enumerate working dir");

        let workdir_entry = workdir_entries
            .iter()
            .find(|e| e.relative_path == "test.txt")
            .expect("test.txt should exist in working dir");

        // THE INVARIANT: blob_id must be identical
        assert_eq!(
            commit_entry.blob_id, workdir_entry.blob_id,
            "blob_id must match between commit and working-dir modes for unchanged files"
        );

        // CRITICAL: relative_path must also be identical for file_id to match.
        // This ensures path normalization is consistent between modes.
        assert_eq!(
            commit_entry.relative_path, workdir_entry.relative_path,
            "relative_path must match between commit and working-dir modes"
        );

        // Also verify the blob_id looks like a valid Git SHA-1 (40 hex chars)
        assert_eq!(
            commit_entry.blob_id.len(),
            40,
            "blob_id should be 40 hex chars (SHA-1)"
        );
        assert!(
            commit_entry.blob_id.chars().all(|c| c.is_ascii_hexdigit()),
            "blob_id should be all hex chars"
        );
    }

    #[test]
    fn test_working_dir_blob_id_matches_commit() {
        let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // Use a stable test artifact that rarely changes (not README.md or Cargo.toml)
        let test_file = "test_artifacts/Colorize.ts";

        // Get commit-mode blob ID for the test file
        let commit_entries =
            enumerate_commit_tree(&repo_path, "HEAD").expect("Failed to enumerate commit");
        let file_commit = commit_entries
            .iter()
            .find(|e| e.relative_path == test_file)
            .expect("test_artifacts/Colorize.ts should exist in commit");

        // Get working-dir blob ID for the test file
        let workdir_entries =
            enumerate_working_directory(&repo_path).expect("Failed to enumerate working dir");
        let file_workdir = workdir_entries
            .iter()
            .find(|e| e.relative_path == test_file)
            .expect("test_artifacts/Colorize.ts should exist in working dir");

        // They should match!
        assert_eq!(
            file_commit.blob_id, file_workdir.blob_id,
            "test_artifacts/Colorize.ts blob_id should match between commit and working-dir modes"
        );
    }
}
