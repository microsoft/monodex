//! Purpose: Test suite for the `git_ops` module.
//! Edit here when: Adding or modifying tests for commit reading, working-directory enumeration, or the package index.
//! Do not edit here for: Production code changes — edit the relevant submodule (`blob_source.rs`, `package_index.rs`, `commit.rs`, `working_dir.rs`).

use super::*;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn test_enumerate_commit_tree_current_repo() {
    let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let blob_source = CommitBlobSource::new(&repo_path, "HEAD".to_string())
        .expect("Failed to create blob source");
    let entries = blob_source.enumerate().expect("Failed to enumerate");
    assert!(!entries.is_empty(), "Should have found some files");
    assert!(entries.iter().any(|e| e.relative_path == "README.md"));
    assert!(entries.iter().any(|e| e.relative_path == "Cargo.toml"));
}

#[test]
fn test_read_blob_content_current_repo() {
    let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let blob_source = CommitBlobSource::new(&repo_path, "HEAD".to_string())
        .expect("Failed to create blob source");
    let entries = blob_source.enumerate().expect("Failed to enumerate");
    let readme = entries
        .iter()
        .find(|e| e.relative_path == "README.md")
        .unwrap();
    let content = blob_source
        .read_content(readme)
        .expect("Failed to read blob");
    let content_str = String::from_utf8_lossy(&content);
    assert!(content_str.contains("Monodex"));
}

#[test]
fn test_build_package_index() {
    let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let blob_source = CommitBlobSource::new(&repo_path, "HEAD".to_string())
        .expect("Failed to create blob source");
    let _index = blob_source
        .build_package_index()
        .expect("Failed to build index");
}

#[test]
fn test_find_package_name_uses_repo_relative_paths() {
    let mut index = PackageIndex::new();
    index.insert_package_name(
        "libraries/node-core-library".to_string(),
        "@rushstack/node-core-library".to_string(),
    );
    index.insert_package_name("".to_string(), "root-package".to_string());

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
    index.insert_package_name(
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
    // Note: Hidden files/folders (dot-prefixed) are skipped during enumeration
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

    // Create a temporary folder
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
    let blob_source =
        CommitBlobSource::new(repo_path, "HEAD".to_string()).expect("Failed to create blob source");
    let commit_entries = blob_source.enumerate().expect("Failed to enumerate commit");
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
    let blob_source = CommitBlobSource::new(&repo_path, "HEAD".to_string())
        .expect("Failed to create blob source");
    let commit_entries = blob_source.enumerate().expect("Failed to enumerate commit");
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

/// Git-tracked files under hidden folders must be indexed.
/// Previously, working-directory crawls skipped files under .github/, .vscode/, etc.
/// even when Git tracked them.
#[test]
fn test_hidden_directory_files_are_indexed() {
    let repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let entries =
        enumerate_working_directory(&repo_path).expect("Failed to enumerate working directory");

    // This repo has .github/workflows/ci.yaml which Git tracks.
    // It must appear in the working directory enumeration.
    let hidden_file = entries
        .iter()
        .find(|e| e.relative_path == ".github/workflows/ci.yaml");
    assert!(
        hidden_file.is_some(),
        ".github/workflows/ci.yaml should be found in working directory enumeration"
    );

    // Also verify it has a valid blob_id
    if let Some(entry) = hidden_file {
        assert_eq!(
            entry.blob_id.len(),
            40,
            "blob_id should be 40 chars: {}",
            entry.blob_id
        );
    }
}

/// Repo whose basename starts with '.' must produce non-empty output.
/// Previously, the root-handling logic diverged between enumerate_working_directory
/// and build_package_index_for_working_dir, causing the latter to return nothing
/// for repos like /tmp/.my-repo/.
#[test]
fn test_repo_with_dot_basename_produces_output() {
    use std::fs;
    use tempfile::TempDir;

    // Create a temporary folder with a dot-prefixed name
    let temp_base = TempDir::new().expect("Failed to create temp dir");
    let dot_repo_path = temp_base.path().join(".my-repo");
    fs::create_dir_all(&dot_repo_path).expect("Failed to create .my-repo dir");

    // Initialize a minimal Git repo
    let git_init = Command::new("git")
        .args(["init"])
        .current_dir(&dot_repo_path)
        .output()
        .expect("Failed to run git init");
    assert!(git_init.status.success(), "git init failed");

    // Configure local user for this repo
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(&dot_repo_path)
        .output()
        .expect("Failed to set user.name");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&dot_repo_path)
        .output()
        .expect("Failed to set user.email");

    // Create and commit a test file
    let test_file = dot_repo_path.join("test.txt");
    fs::write(&test_file, "Hello, World!\n").expect("Failed to write test file");

    Command::new("git")
        .args(["add", "test.txt"])
        .current_dir(&dot_repo_path)
        .output()
        .expect("Failed to run git add");

    let git_commit = Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(&dot_repo_path)
        .output()
        .expect("Failed to run git commit");
    assert!(git_commit.status.success(), "git commit failed");

    // enumerate_working_directory must produce non-empty output
    let entries =
        enumerate_working_directory(&dot_repo_path).expect("Failed to enumerate working directory");
    assert!(
        !entries.is_empty(),
        "enumerate_working_directory should find files in a repo whose basename starts with '.'"
    );
    assert!(
        entries.iter().any(|e| e.relative_path == "test.txt"),
        "test.txt should be found in the enumeration"
    );

    // build_package_index_for_working_dir must also work (even without package.json,
    // it should return an empty index, not error)
    let package_index =
        build_package_index_for_working_dir(&dot_repo_path).expect("Failed to build package index");
    // The index is empty since there's no package.json, but it shouldn't error
    assert!(
        package_index.is_empty(),
        "Package index should be empty (no package.json in test repo)"
    );
}

/// Test that files named like "something-package.json" are not treated as package.json files.
/// This verifies exact filename matching, not substring matching.
#[test]
fn test_package_json_exact_filename_matching() {
    use std::fs;
    use tempfile::TempDir;

    // Create a temporary Git repo
    let temp_repo = TempDir::new().expect("Failed to create temp dir");
    let repo_path = temp_repo.path();

    // Initialize Git repo
    Command::new("git")
        .args(["init"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git init");
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

    // Create a valid package.json at the root
    let real_package_json = repo_path.join("package.json");
    fs::write(&real_package_json, r#"{"name": "real-package"}"#)
        .expect("Failed to write package.json");

    // Create a file named "something-package.json" at the root with valid JSON
    let fake_package_json = repo_path.join("my-package.json");
    fs::write(&fake_package_json, r#"{"name": "fake-package"}"#)
        .expect("Failed to write my-package.json");

    // Create another fake package.json in a subdirectory
    fs::create_dir_all(repo_path.join("subdir")).expect("Failed to create subdir");
    let subdir_fake = repo_path.join("subdir/another-package.json");
    fs::write(&subdir_fake, r#"{"name": "another-fake"}"#)
        .expect("Failed to write another-package.json");

    // Create a real package.json in a subdirectory
    let subdir_real = repo_path.join("subdir/package.json");
    fs::write(&subdir_real, r#"{"name": "subdir-real"}"#)
        .expect("Failed to write subdir/package.json");

    // Stage and commit all files
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git add");
    let git_commit = Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git commit");
    assert!(git_commit.status.success(), "git commit failed");

    // Build the package index
    let package_index =
        build_package_index_for_working_dir(repo_path).expect("Failed to build package index");

    // Verify: only real package.json files are in the index
    assert_eq!(
        package_index.get_package_name(""),
        Some("real-package"),
        "Root package.json should be indexed"
    );
    assert_eq!(
        package_index.get_package_name("subdir"),
        Some("subdir-real"),
        "subdir/package.json should be indexed"
    );
    assert_eq!(
        package_index.len(),
        2,
        "Only 2 package.json files should be indexed, not the *-package.json files"
    );
}

/// Untracked non-ignored files must appear in working-directory enumeration.
/// The working-tree view includes tracked files at their working-tree contents,
/// plus untracked non-ignored files reported by `git status -u`.
#[test]
fn test_untracked_non_ignored_file_appears_in_working_dir_enumeration() {
    use std::fs;
    use tempfile::TempDir;

    // Create a temporary folder
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

    // Create and commit a tracked file
    let tracked_file = repo_path.join("tracked.txt");
    fs::write(&tracked_file, "I am tracked\n").expect("Failed to write tracked file");

    Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git add");

    let git_commit = Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git commit");
    assert!(git_commit.status.success(), "git commit failed");

    // Create an untracked non-ignored file (no .gitignore, so it won't be ignored)
    let untracked_file = repo_path.join("untracked.txt");
    fs::write(&untracked_file, "I am untracked\n").expect("Failed to write untracked file");

    // Enumerate working directory
    let entries =
        enumerate_working_directory(repo_path).expect("Failed to enumerate working directory");

    // Both files must appear
    let tracked_entry = entries
        .iter()
        .find(|e| e.relative_path == "tracked.txt")
        .expect("tracked.txt should appear in working directory enumeration");

    let untracked_entry = entries
        .iter()
        .find(|e| e.relative_path == "untracked.txt")
        .expect("untracked.txt should appear in working directory enumeration");

    // Both must have valid blob IDs
    assert_eq!(
        tracked_entry.blob_id.len(),
        40,
        "tracked file blob_id should be 40 chars"
    );
    assert_eq!(
        untracked_entry.blob_id.len(),
        40,
        "untracked file blob_id should be 40 chars"
    );
}

/// Package index for working directory must resolve packages from untracked package.json files.
/// This verifies that `build_package_index_for_working_dir` reads untracked non-ignored
/// package.json files and resolves files under those packages correctly.
#[test]
fn test_untracked_package_json_resolved_in_working_dir_package_index() {
    use std::fs;
    use tempfile::TempDir;

    // Create a temporary folder
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

    // Create and commit a tracked package with a source file
    let tracked_pkg_dir = repo_path.join("tracked-pkg");
    fs::create_dir_all(&tracked_pkg_dir).expect("Failed to create tracked-pkg dir");
    fs::write(
        tracked_pkg_dir.join("package.json"),
        r#"{"name": "tracked-package"}"#,
    )
    .expect("Failed to write tracked package.json");
    fs::write(tracked_pkg_dir.join("index.ts"), "// tracked\n").expect("Failed to write index.ts");

    Command::new("git")
        .args(["add", "."])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git add");

    let git_commit = Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git commit");
    assert!(git_commit.status.success(), "git commit failed");

    // Create an untracked package folder with its own package.json and source file
    let untracked_pkg_dir = repo_path.join("untracked-pkg");
    fs::create_dir_all(&untracked_pkg_dir).expect("Failed to create untracked-pkg dir");
    fs::write(
        untracked_pkg_dir.join("package.json"),
        r#"{"name": "untracked-package"}"#,
    )
    .expect("Failed to write untracked package.json");
    fs::write(untracked_pkg_dir.join("index.ts"), "// untracked\n")
        .expect("Failed to write untracked index.ts");

    // Build the package index for working directory
    let package_index =
        build_package_index_for_working_dir(repo_path).expect("Failed to build package index");

    // The untracked package must be resolved
    assert_eq!(
        package_index.get_package_name("untracked-pkg"),
        Some("untracked-package"),
        "untracked-pkg/package.json should be indexed with its correct name"
    );

    // The tracked package must also be present (sanity check)
    assert_eq!(
        package_index.get_package_name("tracked-pkg"),
        Some("tracked-package"),
        "tracked-pkg/package.json should be indexed"
    );
}
