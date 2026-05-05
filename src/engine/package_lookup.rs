//! Purpose: Filesystem-only package-name lookup that walks up to find the nearest `package.json`. Used only by `dump-chunks`.
//! Edit here when: Changing the dump-chunks fallback for resolving package names from a working tree.
//! Do not edit here for: Main-crawl package resolution (see `git_ops/`), JSON name extraction (see `git_ops/mod.rs`).

use std::path::Path;

use super::git_ops::extract_package_name_from_bytes;

/// Find the package name for a given source file.
///
/// This walks upwards from the file's directory to find the nearest package.json
/// and extracts the "name" field. If no package.json is found, it uses
/// the relative folder path from the repo root as a fallback identifier.
///
/// # Arguments
///
/// * `file_path` - Path to a source file
/// * `repo_root` - Root of the monorepo (for fallback path generation)
///
/// # Returns
///
/// Package name string (either from package.json or derived from folder structure)
pub fn find_package_name(file_path: &str, repo_root: &str) -> String {
    let path = Path::new(file_path);

    // Start from the file's directory
    let mut current = path.parent().unwrap_or(path);

    // Walk upwards looking for package.json
    loop {
        let package_json = current.join("package.json");

        if package_json.exists() {
            // Found package.json - try to read and parse it
            if let Some(name) = extract_package_name(&package_json) {
                return name;
            }
            // package.json exists but couldn't parse - keep walking up
        }

        // Go to parent
        match current.parent() {
            Some(parent) => current = parent,
            None => break, // Reached root
        }
    }

    // No package.json found - use relative folder path as identifier
    // e.g., "/repo/libs/util/src/helper.ts" -> "libs/util/src"
    strip_to_relative_path(file_path, repo_root)
}

/// Extracts the "name" field from a package.json file.
///
/// Delegates to the shared `extract_package_name_from_bytes` which uses
/// proper JSON parsing (not string search) to handle edge cases.
fn extract_package_name(package_json: &Path) -> Option<String> {
    let content = std::fs::read(package_json).ok()?;
    extract_package_name_from_bytes(&content)
}

/// Converts an absolute path to a relative path from the repo root.
///
/// For files not in a package, uses the folder structure as the identifier.
/// e.g., "/repo/libs/util/src/file.ts" -> "libs/util/src"
fn strip_to_relative_path(file_path: &str, repo_root: &str) -> String {
    let repo_path = Path::new(repo_root);
    let file_path = Path::new(file_path);

    // Try to strip the repo root
    if let Ok(rel) = file_path.strip_prefix(repo_path) {
        // Get the directory part only (remove the filename)
        let dir = rel.parent().unwrap_or(rel);
        // Convert to string, replace backslashes with forward slashes
        dir.to_string_lossy().replace('\\', "/")
    } else {
        // Couldn't strip - use just the folder name
        file_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_name_from_nonexistent_file() {
        // This test just verifies the function doesn't crash
        let result = extract_package_name(Path::new("/nonexistent/package.json"));
        assert!(result.is_none());
    }
}
