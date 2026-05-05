//! Purpose: Subprocess-based reading of the working directory using the `git` CLI to compute Git-compatible blob IDs that respect `.gitattributes` and clean filters.
//! Edit here when: Changing the `git ls-files` / `git status` / `git hash-object` orchestration, the dirty-path detection, batching strategy, or the minimum-Git-version check.
//! Do not edit here for: The `BlobSource` trait or `PackageIndex` type (see `mod.rs`), gix-based commit reading (see `commit.rs`).

use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use super::{FileEntry, PackageIndex, extract_package_name_from_bytes};

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
