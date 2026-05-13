//! User configuration loading and types.
//!
//! Purpose: Define config file schema and loading/validation logic.
//! Edit here when: Adding config file fields, changing validation rules,
//! or modifying how config is discovered and loaded.
//! Do not edit here for: CLI flags (see cli.rs), command handlers (see commands/).

// Field shape is mirrored in schemas/config.schema.json. When adding or renaming fields here, update the JSON Schema in the same change.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::anyhow;

use crate::engine::identifier::validate_catalog;
use crate::engine::system_info::{
    ResolvedEmbeddingConfig, compute_auto_embedding_config, estimate_ram_usage, format_bytes,
    get_physical_core_count,
};
use crate::paths::Paths;

/// Database configuration (LanceDB)
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Optional path to the database directory.
    /// If not specified, defaults to <config_folder>/default-db.
    /// Must be an absolute path; tilde (~) and environment variables ($VAR) are not supported.
    pub path: Option<String>,
}

/// Catalog configuration
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    /// Catalog type: currently only "monorepo" is supported
    pub r#type: String,
    /// Path to scan
    pub path: String,
}

impl CatalogConfig {
    /// Supported catalog types
    const SUPPORTED_TYPES: &'static [&'static str] = &["monorepo"];

    /// Validate that the catalog type is supported
    pub fn validate(&self) -> anyhow::Result<()> {
        if !Self::SUPPORTED_TYPES.contains(&self.r#type.as_str()) {
            anyhow::bail!(
                "Unsupported catalog type '{}'. Supported types: {}",
                self.r#type,
                Self::SUPPORTED_TYPES.join(", ")
            );
        }
        Ok(())
    }
}

/// Embedding model configuration
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingModelConfig {
    /// Number of ONNX model instances (sessions). Primary driver of memory usage.
    /// Allowed values: "auto" or integer >= 1
    #[serde(
        rename = "modelInstances",
        default = "EmbeddingModelConfig::default_model_instances"
    )]
    pub model_instances: EmbeddingSizeValue,

    /// Threads per model instance. CPU tuning only.
    /// Allowed values: "auto" or integer >= 1
    #[serde(
        rename = "threadsPerInstance",
        default = "EmbeddingModelConfig::default_threads_per_instance"
    )]
    pub threads_per_instance: EmbeddingSizeValue,
}

/// A value that can be either "auto" or a specific integer
#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingSizeValue {
    Auto,
    Exact(usize),
}

impl EmbeddingModelConfig {
    fn default_model_instances() -> EmbeddingSizeValue {
        EmbeddingSizeValue::Auto
    }

    fn default_threads_per_instance() -> EmbeddingSizeValue {
        EmbeddingSizeValue::Auto
    }
}

impl Default for EmbeddingModelConfig {
    fn default() -> Self {
        Self {
            model_instances: Self::default_model_instances(),
            threads_per_instance: Self::default_threads_per_instance(),
        }
    }
}

impl<'de> serde::Deserialize<'de> for EmbeddingSizeValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, Visitor};

        struct EmbeddingSizeValueVisitor;

        impl<'de> Visitor<'de> for EmbeddingSizeValueVisitor {
            type Value = EmbeddingSizeValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(r#""auto" or an integer >= 1"#)
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if v == "auto" {
                    Ok(EmbeddingSizeValue::Auto)
                } else {
                    Err(de::Error::custom(r#"expected "auto" or an integer >= 1"#))
                }
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if v >= 1 {
                    Ok(EmbeddingSizeValue::Exact(v as usize))
                } else {
                    Err(de::Error::custom("integer must be >= 1"))
                }
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if v >= 1 {
                    Ok(EmbeddingSizeValue::Exact(v as usize))
                } else {
                    Err(de::Error::custom("integer must be >= 1"))
                }
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                // Accept whole numbers from JSON parsers that serialize integers as floats
                if v >= 1.0 && v.fract() == 0.0 {
                    Ok(EmbeddingSizeValue::Exact(v as usize))
                } else if v < 1.0 {
                    Err(de::Error::custom("integer must be >= 1"))
                } else {
                    Err(de::Error::custom(
                        "expected an integer >= 1, got fractional value",
                    ))
                }
            }
        }

        deserializer.deserialize_any(EmbeddingSizeValueVisitor)
    }
}

/// Configuration file schema (deserialization target).
///
/// This is the shape of the JSON config file. It is separate from the runtime
/// `Config` struct because we need to add paths that are not part of the file.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    pub catalogs: HashMap<String, CatalogConfig>,
    #[serde(rename = "embeddingModel", default)]
    pub embedding_model: EmbeddingModelConfig,
    #[serde(default)]
    pub database: Option<DatabaseConfig>,
}

/// Main configuration with resolved paths.
///
/// This is the runtime configuration passed through the application.
/// It combines the config file contents with the resolved `Paths`.
#[derive(Debug, Clone)]
pub struct Config {
    /// Resolved filesystem paths
    pub paths: Paths,
    /// Catalog definitions from config file
    pub catalogs: HashMap<String, CatalogConfig>,
    /// Embedding model configuration
    pub embedding_model: EmbeddingModelConfig,
    /// Optional database configuration
    pub database: Option<DatabaseConfig>,
}

/// Load config from a paths struct.
/// Validates catalog names and types after parsing.
///
/// Error messages:
/// - File not found: "No config found at <path>. See the README for instructions on creating a config file."
/// - Other IO errors: preserved with context
/// - Parse/validation errors: preserved
pub fn load_config(paths: Paths) -> anyhow::Result<Config> {
    let config_path = paths.config_file();
    let content = std::fs::read_to_string(&config_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!(
                "No config found at {}. See the README for instructions on creating a config file.",
                config_path.display()
            )
        } else {
            anyhow!(
                "Failed to read config file {}: {}",
                config_path.display(),
                e
            )
        }
    })?;

    // Parse JSONC (JSON with comments, per Rush Stack convention)
    let stripped = json_comments::StripComments::new(content.as_bytes());
    let config_file: ConfigFile = serde_json::from_reader(stripped)
        .map_err(|e| anyhow!("Failed to parse config file: {}", e))?;

    // Validate catalog names and types
    for (name, catalog) in &config_file.catalogs {
        validate_catalog(name)
            .map_err(|e| anyhow!("Invalid catalog name '{}' in config: {}", name, e))?;
        catalog
            .validate()
            .map_err(|e| anyhow!("Invalid catalog '{}': {}", name, e))?;
    }

    Ok(Config {
        paths,
        catalogs: config_file.catalogs,
        embedding_model: config_file.embedding_model,
        database: config_file.database,
    })
}

/// Validate that a config-file path setting is an absolute, literal path.
///
/// Rejects tilde expansion ('~'), environment variable substitution ('$'),
/// and relative paths so config values mean the same thing regardless of
/// the user's shell, working directory, or platform.
pub fn validate_config_path(field_name: &str, value: &str) -> anyhow::Result<PathBuf> {
    if value.starts_with('~') {
        anyhow::bail!(
            "Invalid {} '{}': '~' is not supported. Provide an absolute path.",
            field_name,
            value
        );
    }
    if value.contains('$') {
        anyhow::bail!(
            "Invalid {} '{}': environment variable substitution is not supported. Provide an absolute path.",
            field_name,
            value
        );
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        anyhow::bail!(
            "Invalid {} '{}': must be an absolute path.",
            field_name,
            value
        );
    }
    Ok(path)
}

/// Resolve the database path from config.
///
/// - If `database.path` is specified in config, validates it as an absolute path and returns it.
/// - Otherwise returns `<config_folder>/default-db`.
pub fn resolve_database_path(config: &Config) -> anyhow::Result<PathBuf> {
    // Check if database.path is specified in config
    if let Some(db_config) = &config.database
        && let Some(path) = &db_config.path
    {
        return validate_config_path("database.path", path);
    }

    // Default: <config_folder>/default-db
    let result = config.paths.config_folder.join("default-db");
    Ok(result)
}

// ============================================================================
// B.2: Embedding configuration resolution
// ============================================================================

/// Resolve embedding configuration from config file, applying "auto" heuristic if needed.
/// Returns ResolvedEmbeddingConfig with all resolved values including memory info for warnings.
pub fn resolve_embedding_config(config: &EmbeddingModelConfig) -> ResolvedEmbeddingConfig {
    match (&config.model_instances, &config.threads_per_instance) {
        (EmbeddingSizeValue::Auto, EmbeddingSizeValue::Auto) => {
            // Both auto: compute from system properties
            match compute_auto_embedding_config() {
                Ok(resolved) => {
                    println!(
                        "Auto-detected embedding config: {} instances × {} threads",
                        resolved.model_instances, resolved.threads_per_instance
                    );
                    if resolved.cgroup_limited {
                        println!(
                            "  (Cgroup memory limit detected: {})",
                            format_bytes(resolved.total_ram)
                        );
                    }
                    resolved
                }
                Err(e) => {
                    eprintln!("Warning: Failed to auto-detect embedding config: {}", e);
                    eprintln!(
                        "Using fallback: 1 instance × {} threads",
                        std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(1)
                    );
                    ResolvedEmbeddingConfig {
                        model_instances: 1,
                        threads_per_instance: std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(1),
                        total_ram: 0,
                        available_ram: 0,
                        estimated_ram_usage: estimate_ram_usage(1),
                        cgroup_limited: false,
                    }
                }
            }
        }
        (EmbeddingSizeValue::Auto, EmbeddingSizeValue::Exact(threads)) => {
            // Auto instances, explicit threads
            match compute_auto_embedding_config() {
                Ok(resolved) => {
                    println!(
                        "Auto-detected model instances: {} (using explicit {} threads/instance)",
                        resolved.model_instances, threads
                    );
                    ResolvedEmbeddingConfig {
                        threads_per_instance: *threads,
                        ..resolved
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Failed to auto-detect embedding config: {}", e);
                    eprintln!("Using fallback: 1 instance × {} threads", *threads);
                    ResolvedEmbeddingConfig {
                        model_instances: 1,
                        threads_per_instance: *threads,
                        total_ram: 0,
                        available_ram: 0,
                        estimated_ram_usage: estimate_ram_usage(1),
                        cgroup_limited: false,
                    }
                }
            }
        }
        (EmbeddingSizeValue::Exact(instances), EmbeddingSizeValue::Auto) => {
            // Explicit instances, auto threads
            let physical_cores = get_physical_core_count();
            let threads = std::cmp::max(1, physical_cores / instances);
            println!(
                "Using explicit {} model instances (auto-detected {} threads/instance)",
                instances, threads
            );
            // Get memory info via compute_auto_embedding_config for cgroup-aware values
            let memory_info = compute_auto_embedding_config()
                .map(|resolved| {
                    (
                        resolved.total_ram,
                        resolved.available_ram,
                        resolved.cgroup_limited,
                    )
                })
                .unwrap_or((0, 0, false));
            ResolvedEmbeddingConfig {
                model_instances: *instances,
                threads_per_instance: threads,
                total_ram: memory_info.0,
                available_ram: memory_info.1,
                estimated_ram_usage: estimate_ram_usage(*instances),
                cgroup_limited: memory_info.2,
            }
        }
        (EmbeddingSizeValue::Exact(instances), EmbeddingSizeValue::Exact(threads)) => {
            // Both explicit
            println!(
                "Using explicit config: {} instances × {} threads/instance",
                instances, threads
            );
            // Get memory info via compute_auto_embedding_config for cgroup-aware values
            let memory_info = compute_auto_embedding_config()
                .map(|resolved| {
                    (
                        resolved.total_ram,
                        resolved.available_ram,
                        resolved.cgroup_limited,
                    )
                })
                .unwrap_or((0, 0, false));
            ResolvedEmbeddingConfig {
                model_instances: *instances,
                threads_per_instance: *threads,
                total_ram: memory_info.0,
                available_ram: memory_info.1,
                estimated_ram_usage: estimate_ram_usage(*instances),
                cgroup_limited: memory_info.2,
            }
        }
    }
}

/// Print memory status and warning if estimated usage exceeds available RAM.
pub fn print_memory_warning(resolved: &ResolvedEmbeddingConfig) {
    // Skip warning if we couldn't get memory info
    if resolved.available_ram == 0 {
        eprintln!("Warning: Could not get memory info for warning check");
        return;
    }

    println!(
        "Currently available system RAM: {}",
        format_bytes(resolved.available_ram)
    );
    println!(
        "Estimated embedding RAM usage: {} ({} instance{})",
        format_bytes(resolved.estimated_ram_usage),
        resolved.model_instances,
        if resolved.model_instances > 1 {
            "s"
        } else {
            ""
        }
    );

    if resolved.estimated_ram_usage > resolved.available_ram {
        let excess_pct =
            ((resolved.estimated_ram_usage as f64 / resolved.available_ram as f64) - 1.0) * 100.0;
        eprintln!();
        eprintln!(
            "🚨 Warning: estimate exceeds available RAM by {:.0}%.",
            excess_pct
        );
        eprintln!("   Consider adjusting \"embeddingModel.modelInstances\" or");
        eprintln!("   \"embeddingModel.threadsPerInstance\" in monodex-config.json");
        eprintln!("   Suggestion: start with modelInstances = 1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_catalog_config_validates_monorepo_type() {
        let config = CatalogConfig {
            r#type: "monorepo".to_string(),
            path: "/some/path".to_string(),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_catalog_config_rejects_unsupported_type() {
        let config = CatalogConfig {
            r#type: "folder".to_string(),
            path: "/some/path".to_string(),
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Unsupported catalog type 'folder'")
        );
        assert!(err.to_string().contains("Supported types: monorepo"));
    }

    #[test]
    fn test_catalog_config_rejects_unknown_type() {
        let config = CatalogConfig {
            r#type: "unknown".to_string(),
            path: "/some/path".to_string(),
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Unsupported catalog type 'unknown'")
        );
    }

    #[test]
    fn test_load_config_validates_catalog_types() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        // Config with invalid catalog type
        writeln!(
            file,
            r#"{{
                "catalogs": {{
                    "test": {{
                        "type": "invalid",
                        "path": "/tmp"
                    }}
                }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let result = load_config(paths);
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Invalid catalog 'test'"));
        assert!(
            err.to_string()
                .contains("Unsupported catalog type 'invalid'")
        );
    }

    #[test]
    fn test_load_config_accepts_monorepo_type() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        assert_eq!(config.catalogs.get("sparo").unwrap().r#type, "monorepo");
    }

    #[test]
    fn test_load_config_accepts_database_path() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }},
                "database": {{ "path": "/custom/db" }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        assert!(config.database.is_some());
        let db_config = config.database.unwrap();
        assert_eq!(db_config.path, Some("/custom/db".to_string()));
    }

    #[test]
    fn test_load_config_database_section_optional() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        assert!(config.database.is_none());
    }

    #[test]
    fn test_resolve_database_path_rejects_tilde() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{}},
                "database": {{ "path": "~/my-db" }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        let result = resolve_database_path(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("'~' is not supported"),
            "Expected error about tilde, got: {}",
            err
        );
        assert!(
            err.contains("database.path"),
            "Expected error to mention field name, got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_database_path_rejects_env_var() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{}},
                "database": {{ "path": "$HOME/my-db" }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        let result = resolve_database_path(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("environment variable substitution is not supported"),
            "Expected error about env var, got: {}",
            err
        );
        assert!(
            err.contains("database.path"),
            "Expected error to mention field name, got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_database_path_rejects_relative_path() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{}},
                "database": {{ "path": "./my-db" }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        let result = resolve_database_path(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("must be an absolute path"),
            "Expected error about absolute path, got: {}",
            err
        );
        assert!(
            err.contains("database.path"),
            "Expected error to mention field name, got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_database_path_accepts_absolute_path() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{}},
                "database": {{ "path": "/custom/db" }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        let path = resolve_database_path(&config).unwrap();
        assert_eq!(path, PathBuf::from("/custom/db"));
    }

    #[test]
    fn test_resolve_database_path_defaults_to_config_folder() {
        // When no database.path, should use <config_folder>/default-db
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{}}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        let path = resolve_database_path(&config).unwrap();

        // Should be <config_folder>/default-db
        assert_eq!(path, dir.path().join("default-db"));
    }

    #[test]
    fn test_resolve_database_path_config_without_database_section() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "catalogs": {{}}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        let path = resolve_database_path(&config).unwrap();

        // Should be <config_folder>/default-db
        assert_eq!(path, dir.path().join("default-db"));
    }

    #[test]
    fn test_config_rejects_unknown_fields() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        // Config with an unknown field "qdrant" (old Qdrant-era config)
        writeln!(
            file,
            r#"{{
                "catalogs": {{}},
                "qdrant": {{
                    "url": "http://localhost:6333",
                    "collection": "monodex"
                }}
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let result = load_config(paths);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown field"),
            "Expected error about unknown field, got: {}",
            err
        );
    }

    #[test]
    fn test_load_config_missing_file_centralized_error() {
        // Test that the "config file not found" error uses the centralized wording
        let temp_dir = tempdir().unwrap();
        let paths = Paths::for_test(temp_dir.path().into());
        let result = load_config(paths);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();

        // Exact match on the centralized error message format
        assert!(
            err.starts_with("No config found at"),
            "Expected error to start with 'No config found at', got: {}",
            err
        );
        assert!(
            err.contains("See the README for instructions on creating a config file."),
            "Expected error to contain README hint, got: {}",
            err
        );
    }

    #[test]
    fn test_load_config_parses_jsonc_with_line_comments() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("monodex-config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        // Config with // line comments (JSONC)
        writeln!(
            file,
            r#"{{
                // This is a line comment
                "catalogs": {{
                    // Another comment
                    "sparo": {{
                        "type": "monorepo", // inline comment
                        "path": "/tmp/sparo"
                    }}
                }}
                // Final comment
            }}"#
        )
        .unwrap();

        let paths = Paths::for_test(dir.path().into());
        let config = load_config(paths).unwrap();
        assert_eq!(config.catalogs.get("sparo").unwrap().r#type, "monorepo");
    }

    #[test]
    fn test_example_config_validates_against_schema() {
        use jsonschema::Validator;

        // Load the schema
        let schema_path = "schemas/config.schema.json";
        let schema_str = std::fs::read_to_string(schema_path)
            .expect("Failed to read config.schema.json - run from project root");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_str).expect("Failed to parse config.schema.json as JSON");

        // Compile the schema
        let validator = Validator::new(&schema).expect("Failed to compile JSON schema");

        // Load the example config (JSONC - has comments)
        let example_path = "examples/monodex-config.json";
        let example_str = std::fs::read_to_string(example_path)
            .expect("Failed to read examples/monodex-config.json - run from project root");

        // Strip comments and parse (same approach as load_config)
        let stripped = json_comments::StripComments::new(example_str.as_bytes());
        let example: serde_json::Value = serde_json::from_reader(stripped)
            .expect("Failed to parse examples/monodex-config.json as JSON");

        assert!(
            validator.is_valid(&example),
            "examples/monodex-config.json does not validate against schema"
        );
    }
}
