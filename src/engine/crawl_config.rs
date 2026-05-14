//! Purpose: Load and compile `monodex-crawl.json` (file types, exclude/keep patterns) and evaluate `should_crawl` / `get_strategy`.
//! Edit here when: Adding crawl-config fields, changing pattern evaluation, updating the embedded default, or adding `ChunkingStrategy` variants.
//! Do not edit here for: Chunking algorithms (see `chunker.rs` and submodules), app-level config (see `app/config.rs`).

// Field shape is mirrored in schemas/crawl.schema.json. When adding or renaming fields here, update the JSON Schema in the same change.

/// Chunking strategy enumeration.
///
/// Determines how a file is split into chunks for indexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkingStrategy {
    /// TypeScript files - AST-based semantic chunking
    TypeScript,

    /// Markdown files - Split by heading hierarchy
    Markdown,

    /// Simple line-based chunking
    LineBased,

    /// Skip this file (don't index)
    Skip,
}

use anyhow::{Result, anyhow};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Crawl configuration loaded from `monodex-crawl.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrawlConfig {
    /// Schema URL for editor validation (ignored at runtime)
    #[serde(default, rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,

    /// Config schema version (must be 1)
    pub version: u32,

    /// File suffix → chunking strategy mapping
    /// Keys are file suffixes like ".ts", ".md"
    /// Values are strategy identifiers like "typescript", "markdown"
    #[serde(rename = "fileTypes")]
    pub file_types: HashMap<String, String>,

    /// Glob patterns for paths to exclude from crawling
    /// Directory patterns (ending in "/") match any path under that directory
    #[serde(rename = "patternsToExclude")]
    pub patterns_to_exclude: Vec<String>,

    /// Glob patterns that override exclusion (higher precedence)
    /// Directory patterns (ending in "/") match any path under that directory
    #[serde(rename = "patternsToKeep")]
    pub patterns_to_keep: Vec<String>,
}

/// Compiled crawl configuration with glob sets ready for matching.
#[derive(Debug, Clone)]
pub struct CompiledCrawlConfig {
    /// Original config
    pub config: CrawlConfig,

    /// Compiled exclusion patterns (non-directory patterns)
    exclude_set: GlobSet,

    /// Compiled keep patterns (non-directory patterns)
    keep_set: GlobSet,

    /// Directory prefixes for exclusion (patterns ending in "/")
    exclude_dirs: Vec<String>,

    /// Directory prefixes for keep (patterns ending in "/")
    keep_dirs: Vec<String>,
}

impl CrawlConfig {
    /// Parse config from JSON string.
    pub fn from_json(json: &str) -> Result<Self> {
        let config: CrawlConfig = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate config fields.
    pub fn validate(&self) -> Result<()> {
        // Validate version
        if self.version != 1 {
            return Err(anyhow!(
                "Unsupported config version: {} (expected 1)",
                self.version
            ));
        }

        // Validate file_types strategies
        for (suffix, strategy) in &self.file_types {
            if !is_valid_strategy(strategy) {
                return Err(anyhow!(
                    "Unknown chunking strategy '{}' for suffix '{}'. Valid strategies: typescript, markdown, lineBased",
                    strategy,
                    suffix
                ));
            }
            // Validate suffix format (should start with .)
            if !suffix.starts_with('.') {
                return Err(anyhow!(
                    "Invalid file suffix '{}': must start with '.' (e.g., '.ts')",
                    suffix
                ));
            }
        }

        // Validate glob patterns compile
        for pattern in &self.patterns_to_exclude {
            Glob::new(pattern)
                .map_err(|e| anyhow!("Invalid exclude pattern '{}': {}", pattern, e))?;
        }

        for pattern in &self.patterns_to_keep {
            Glob::new(pattern).map_err(|e| anyhow!("Invalid keep pattern '{}': {}", pattern, e))?;
        }

        Ok(())
    }

    /// Compile glob patterns into a matcher.
    pub fn compile(&self) -> Result<CompiledCrawlConfig> {
        let mut exclude_builder = GlobSetBuilder::new();
        let mut exclude_dirs = Vec::new();

        for pattern in &self.patterns_to_exclude {
            if pattern.ends_with('/') {
                // Directory pattern: store as prefix matcher
                exclude_dirs.push(pattern.clone());
            } else {
                exclude_builder.add(Glob::new(pattern)?);
            }
        }

        let mut keep_builder = GlobSetBuilder::new();
        let mut keep_dirs = Vec::new();

        for pattern in &self.patterns_to_keep {
            if pattern.ends_with('/') {
                // Directory pattern: store as prefix matcher
                keep_dirs.push(pattern.clone());
            } else {
                keep_builder.add(Glob::new(pattern)?);
            }
        }

        Ok(CompiledCrawlConfig {
            config: self.clone(),
            exclude_set: exclude_builder.build()?,
            keep_set: keep_builder.build()?,
            exclude_dirs,
            keep_dirs,
        })
    }
}

impl CompiledCrawlConfig {
    /// Check if a repo-relative path should be crawled.
    ///
    /// Evaluation rule:
    /// ```text
    /// shouldCrawl = matchesFileType && (matchesPatternsToKeep || !matchesPatternsToExclude)
    /// ```
    pub fn should_crawl(&self, repo_relative_path: &str) -> bool {
        // First: check if file type is supported
        if !self.matches_file_type(repo_relative_path) {
            return false;
        }

        // Second: check exclusion with keep override
        let matches_exclude = self.matches_exclude(repo_relative_path);
        let matches_keep = self.matches_keep(repo_relative_path);

        matches_keep || !matches_exclude
    }

    /// Check if file matches a configured file type.
    fn matches_file_type(&self, path: &str) -> bool {
        // Match against suffixes (case-sensitive, v1)
        for suffix in self.config.file_types.keys() {
            if path.ends_with(suffix) {
                return true;
            }
        }
        false
    }

    /// Check if path matches exclusion patterns.
    fn matches_exclude(&self, path: &str) -> bool {
        // Check glob patterns
        if self.exclude_set.is_match(path) {
            return true;
        }
        // Check directory prefixes
        for dir in &self.exclude_dirs {
            // Directory patterns match if:
            // 1. Path starts with the directory (e.g., "lib/example.ts" matches "lib/")
            // 2. Path contains the directory with leading slash (e.g., "path/to/lib/file.ts" matches "lib/")
            if path.starts_with(dir) || path.contains(&format!("/{}", dir)) {
                return true;
            }
        }
        false
    }

    /// Check if path matches keep patterns.
    fn matches_keep(&self, path: &str) -> bool {
        // Check glob patterns
        if self.keep_set.is_match(path) {
            return true;
        }
        // Check directory prefixes
        for dir in &self.keep_dirs {
            // Directory patterns match if:
            // 1. Path starts with the directory (e.g., "src/example.ts" matches "src/")
            // 2. Path contains the directory with leading slash (e.g., "path/to/src/file.ts" matches "src/")
            if path.starts_with(dir) || path.contains(&format!("/{}", dir)) {
                return true;
            }
        }
        false
    }

    /// Get the chunking strategy for a file.
    ///
    /// Returns the strategy for the file's extension, or `ChunkingStrategy::Skip`
    /// if the file type is not configured.
    pub fn get_strategy(&self, repo_relative_path: &str) -> ChunkingStrategy {
        for (suffix, strategy) in &self.config.file_types {
            if repo_relative_path.ends_with(suffix) {
                return match strategy.as_str() {
                    "typescript" => ChunkingStrategy::TypeScript,
                    "markdown" => ChunkingStrategy::Markdown,
                    "lineBased" => ChunkingStrategy::LineBased,
                    _ => ChunkingStrategy::Skip, // Unknown strategy string
                };
            }
        }
        ChunkingStrategy::Skip
    }
}

/// Check if a strategy name is valid.
fn is_valid_strategy(strategy: &str) -> bool {
    matches!(strategy, "typescript" | "markdown" | "lineBased")
}

// =============================================================================
// Config Discovery
// =============================================================================

/// Embedded default crawl config.
/// This is used when no repo-local or user-global config is found.
const DEFAULT_CRAWL_CONFIG_JSON: &str = r#"{
    "version": 1,
    "fileTypes": {
        ".ts": "typescript",
        ".tsx": "typescript",
        ".md": "markdown",
        ".yml": "lineBased",
        ".yaml": "lineBased",
        ".txt": "lineBased",
        ".css": "lineBased",
        ".scss": "lineBased"
    },
    "patternsToExclude": [
        "node_modules/",
        "dist/",
        "build/",
        "lib/",
        "lib-commonjs/",
        "lib-esm/",
        "lib-esnext/",
        "lib-amd/",
        "lib-dts/",
        "temp/",
        ".cache/",
        ".rush/temp/",
        "common/temp/",
        "common/deploy/",
        ".docusaurus/",
        "**/*.snap",
        "**/*.test.ts",
        "**/*.spec.ts",
        "**/*.test.tsx",
        "**/*.spec.tsx",
        "**/*.js",
        "**/*.jsx",
        "**/*.cjs",
        "**/*.mjs",
        "**/*.json",
        "**/package-lock.json",
        "**/pnpm-lock.yaml",
        "**/yarn.lock",
        "**/*.lock",
        "**/*.tsbuildinfo"
    ],
    "patternsToKeep": [
        "src/",
        "test/"
    ]
}"#;

/// Load crawl config with discovery precedence.
///
/// Precedence order (first found wins):
/// 1. `<repo-root>/monodex-crawl.json` (repo-local)
/// 2. `~/.monodex/monodex-crawl-config.json` (user-global)
/// 3. Embedded default (compiled into binary)
///
/// No merging is performed - exactly one config is used.
pub fn load_crawl_config(
    paths: &crate::paths::Paths,
    repo_path: Option<&Path>,
) -> Result<CrawlConfig> {
    // Try repo-local config first
    if let Some(repo) = repo_path {
        let repo_local_path = repo.join("monodex-crawl.json");
        if repo_local_path.exists() {
            let content = std::fs::read_to_string(&repo_local_path).map_err(|e| {
                anyhow!(
                    "Failed to read repo-local config {:?}: {}",
                    repo_local_path,
                    e
                )
            })?;
            eprintln!("Using repo-local crawl config: {:?}", repo_local_path);
            return CrawlConfig::from_json(&content);
        }
    }

    // Try user-global config
    let user_global_path = paths.crawl_config();
    if user_global_path.exists() {
        let content = std::fs::read_to_string(&user_global_path).map_err(|e| {
            anyhow!(
                "Failed to read user-global config {:?}: {}",
                user_global_path,
                e
            )
        })?;
        eprintln!("Using user-global crawl config: {:?}", user_global_path);
        return CrawlConfig::from_json(&content);
    }

    // Fall back to embedded default
    eprintln!("Using embedded default crawl config");
    CrawlConfig::from_json(DEFAULT_CRAWL_CONFIG_JSON)
}

/// Load and compile crawl config in one step.
///
/// This is a convenience function that loads the config and compiles it
/// for use in crawl operations.
pub fn load_compiled_crawl_config(
    paths: &crate::paths::Paths,
    repo_path: Option<&Path>,
) -> Result<CompiledCrawlConfig> {
    let config = load_crawl_config(paths, repo_path)?;
    config.compile()
}

/// Get the embedded default crawl config.
///
/// Useful for debugging or generating a starter config file.
#[allow(dead_code)]
pub fn get_default_crawl_config() -> CrawlConfig {
    CrawlConfig::from_json(DEFAULT_CRAWL_CONFIG_JSON)
        .expect("Embedded default config should be valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CrawlConfig {
        CrawlConfig::from_json(
            r#"{
                "version": 1,
                "fileTypes": {
                    ".ts": "typescript",
                    ".tsx": "typescript",
                    ".md": "markdown",
                    ".yaml": "lineBased"
                },
                "patternsToExclude": [
                    "node_modules/",
                    "**/*.test.ts",
                    "**/*.spec.ts"
                ],
                "patternsToKeep": [
                    "src/",
                    "test/"
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn test_config_validation() {
        let config = test_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_version() {
        let result = CrawlConfig::from_json(
            r#"{
                "version": 2,
                "fileTypes": {".ts": "typescript"},
                "patternsToExclude": [],
                "patternsToKeep": []
            }"#,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unsupported config version")
        );
    }

    #[test]
    fn test_invalid_strategy() {
        let result = CrawlConfig::from_json(
            r#"{
                "version": 1,
                "fileTypes": {".ts": "unknownStrategy"},
                "patternsToExclude": [],
                "patternsToKeep": []
            }"#,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unknown chunking strategy")
        );
    }

    #[test]
    fn test_invalid_suffix() {
        let result = CrawlConfig::from_json(
            r#"{
                "version": 1,
                "fileTypes": {"ts": "typescript"},
                "patternsToExclude": [],
                "patternsToKeep": []
            }"#,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must start with '.'")
        );
    }

    #[test]
    fn test_invalid_glob() {
        let result = CrawlConfig::from_json(
            r#"{
                "version": 1,
                "fileTypes": {".ts": "typescript"},
                "patternsToExclude": ["[invalid"],
                "patternsToKeep": []
            }"#,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid exclude pattern")
        );
    }

    #[test]
    fn test_unknown_field_rejected() {
        let result = CrawlConfig::from_json(
            r#"{
                "version": 1,
                "fileTypes": {".ts": "typescript"},
                "patternsToExclude": [],
                "patternsToKeep": [],
                "unknownField": "value"
            }"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_file_type_matching() {
        let compiled = test_config().compile().unwrap();

        // File type matches
        assert!(compiled.matches_file_type("src/index.ts"));
        assert!(compiled.matches_file_type("src/App.tsx"));
        assert!(compiled.matches_file_type("README.md"));
        assert!(compiled.matches_file_type("config.yaml"));

        // File type doesn't match
        assert!(!compiled.matches_file_type("image.png"));
        assert!(!compiled.matches_file_type("package.json")); // .json not in fileTypes
    }

    #[test]
    fn test_should_crawl_basic() {
        let compiled = test_config().compile().unwrap();

        // Supported file type, not excluded
        assert!(compiled.should_crawl("src/index.ts"));
        assert!(compiled.should_crawl("README.md"));
    }

    #[test]
    fn test_should_crawl_exclusion() {
        let compiled = test_config().compile().unwrap();

        // node_modules excluded
        assert!(!compiled.should_crawl("node_modules/package/index.ts"));

        // test files excluded
        assert!(!compiled.should_crawl("utils.test.ts"));
        assert!(!compiled.should_crawl("api.spec.ts"));
    }

    #[test]
    fn test_should_crawl_keep_override() {
        let compiled = test_config().compile().unwrap();

        // src/ overrides exclusion - test file in src is kept
        assert!(compiled.should_crawl("src/utils.test.ts"));

        // But still must match file type
        assert!(!compiled.should_crawl("src/image.png"));
    }

    #[test]
    fn test_should_crawl_unsupported_type() {
        let compiled = test_config().compile().unwrap();

        // Unsupported file types are never crawled, even with keep pattern
        assert!(!compiled.should_crawl("src/image.png"));
        assert!(!compiled.should_crawl("src/style.css"));
    }

    #[test]
    fn test_get_strategy() {
        let compiled = test_config().compile().unwrap();

        assert_eq!(
            compiled.get_strategy("src/index.ts"),
            ChunkingStrategy::TypeScript
        );
        assert_eq!(
            compiled.get_strategy("src/App.tsx"),
            ChunkingStrategy::TypeScript
        );
        assert_eq!(
            compiled.get_strategy("README.md"),
            ChunkingStrategy::Markdown
        );
        assert_eq!(
            compiled.get_strategy("config.yaml"),
            ChunkingStrategy::LineBased
        );
        assert_eq!(compiled.get_strategy("image.png"), ChunkingStrategy::Skip);
    }

    #[test]
    fn test_embedded_default_config_is_valid() {
        let config = get_default_crawl_config();
        assert!(config.validate().is_ok());

        let compiled = config.compile().unwrap();

        // Should match TypeScript files
        assert!(compiled.should_crawl("src/index.ts"));

        // Should exclude node_modules
        assert!(!compiled.should_crawl("node_modules/some-package/index.ts"));

        // Should exclude test files (but src/test files override)
        assert!(!compiled.should_crawl("utils.test.ts"));
        assert!(compiled.should_crawl("src/utils.test.ts"));
    }

    #[test]
    fn test_load_crawl_config_uses_embedded_default() {
        // When no repo path and no user-global config, use embedded default
        let temp_dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::for_test(temp_dir.path().into());
        let config = load_crawl_config(&paths, None).unwrap();
        assert_eq!(config.version, 1);
        assert!(config.file_types.contains_key(".ts"));
    }

    #[test]
    fn test_load_compiled_crawl_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::for_test(temp_dir.path().into());
        let compiled = load_compiled_crawl_config(&paths, None).unwrap();
        assert!(compiled.should_crawl("src/index.ts"));
        assert!(!compiled.should_crawl("node_modules/example.ts"));
    }

    #[test]
    fn test_crawl_config_accepts_schema_field() {
        let result = CrawlConfig::from_json(
            r#"{
                "$schema": "https://example.com/schemas/monodex-crawl.json",
                "version": 1,
                "fileTypes": {".ts": "typescript"},
                "patternsToExclude": [],
                "patternsToKeep": []
            }"#,
        );
        assert!(
            result.is_ok(),
            "CrawlConfig with $schema field should be accepted"
        );
    }

    #[test]
    fn test_example_crawl_config_user_global_validates_against_schema() {
        use jsonschema::Validator;
        use std::fs;

        // Load the schema
        let schema_path = "schemas/crawl.schema.json";
        let schema_str = fs::read_to_string(schema_path)
            .expect("Failed to read crawl.schema.json - run from project root");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_str).expect("Failed to parse crawl.schema.json as JSON");

        // Compile the schema
        let validator = Validator::new(&schema).expect("Failed to compile JSON schema");

        // Load and validate the user-global crawl config example
        let example_path = "examples/monodex-crawl-config.json";
        let example_str = fs::read_to_string(example_path)
            .expect("Failed to read monodex-crawl-config.json - run from project root");
        let example: serde_json::Value = serde_json::from_str(&example_str)
            .expect("Failed to parse monodex-crawl-config.json as JSON");

        assert!(
            validator.is_valid(&example),
            "Example monodex-crawl-config.json does not validate against schema"
        );
    }
}
