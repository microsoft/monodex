//! Purpose: package-name extraction from `package.json` content, and the `PackageIndex` lookup structure built from parsed names.
//! Edit here when: Changing how package names are extracted from package.json files, or modifying the PackageIndex lookup structure.
//! Do not edit here for: BlobSource abstraction (see `blob_source.rs`), commit traversal (see `commit.rs`), working-directory reading (see `working_dir.rs`).

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Lookup structure mapping directory paths to package names.
///
/// Built from all package.json files in the source tree, used to resolve
/// package names for files based on their containing directory.
pub struct PackageIndex {
    package_name_by_dir: HashMap<String, String>,
}

impl PackageIndex {
    pub fn new() -> Self {
        Self {
            package_name_by_dir: HashMap::new(),
        }
    }

    /// Find the package name for a file by searching upward from its directory.
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

    /// Insert a package name for a directory path.
    #[allow(dead_code)]
    pub(super) fn insert_package_name(&mut self, dir_path: String, name: String) {
        self.package_name_by_dir.insert(dir_path, name);
    }

    /// Get the package name for a specific directory (exact match, no upward search).
    #[allow(dead_code)]
    pub(super) fn get_package_name(&self, dir_path: &str) -> Option<&str> {
        self.package_name_by_dir.get(dir_path).map(String::as_str)
    }

    /// Return the number of packages in the index.
    #[allow(dead_code)]
    pub(super) fn len(&self) -> usize {
        self.package_name_by_dir.len()
    }

    /// Return true if the index is empty.
    #[allow(dead_code)]
    pub(super) fn is_empty(&self) -> bool {
        self.package_name_by_dir.is_empty()
    }
}

impl Default for PackageIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct PackageJsonName {
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
