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
use crate::engine::{
    ResolvedEmbeddingConfig, compute_auto_embedding_config, estimate_ram_usage, format_bytes,
    get_physical_core_count,
};
use crate::paths::Paths;

/// Database configuration (LanceDB)
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Optional path to the database folder.
    /// If not specified, defaults to `./default-db` (resolved against the config folder).
    /// May be an absolute path, or a relative path starting with `./` or `../`
    /// (resolved against the folder containing `monodex-config.json`).
    /// Tilde (`~`) and environment variables (`$VAR`) are not supported.
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
    /// Schema URL for editor validation (ignored at runtime)
    #[serde(default, rename = "$schema", skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)]
    pub schema: Option<String>,
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

/// Normalize a path by resolving `.` and `..` components without touching the filesystem.
///
/// This produces the same result as `Path::canonicalize` for well-formed paths
/// but without requiring the path to exist on disk.
///
/// # Panics
///
/// Does not panic, but callers should note that if `path` is not absolute,
/// leading `..` components are preserved (not truncated against a hypothetical
/// root), which may produce a path that climbs above the filesystem root on
/// the platform in question. In practice, `validate_config_path` only calls
/// this with `config_folder.join(relative_path)`, which is always absolute.
fn normalize_path(path: &std::path::Path) -> PathBuf {
    debug_assert!(
        path.is_absolute(),
        "normalize_path expects an absolute path; relative paths may produce incorrect results"
    );

    let mut resolved = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => { /* skip */ }
            std::path::Component::ParentDir => {
                if resolved.pop() {
                    // removed the last component
                } else {
                    resolved.push(comp);
                }
            }
            comp => resolved.push(comp),
        }
    }
    resolved
}

/// Validate that a config-file path setting is an absolute or dot-relative path.
///
/// Absolute paths are returned as-is. Relative paths beginning with `./` or
/// `../` are resolved against the folder containing `monodex-config.json`
/// (the config folder). Bare names (no leading `./` or `../`), tilde
/// expansion (`~`), and environment variable substitution (`$VAR`) are
/// rejected so config values mean the same thing regardless of the user's
/// shell, working directory, or platform.
pub fn validate_config_path(
    field_name: &str,
    value: &str,
    config_folder: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    if value.starts_with('~') {
        anyhow::bail!(
            "Invalid {} '{}': tilde (~) is not supported. \
             Must be an absolute path, or a relative path starting with './' or '../' \
             (resolved against the config folder).",
            field_name,
            value
        );
    }
    if value.contains('$') {
        anyhow::bail!(
            "Invalid {} '{}': environment variable substitution is not supported. \
             Must be an absolute path, or a relative path starting with './' or '../' \
             (resolved against the config folder).",
            field_name,
            value
        );
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else if value.starts_with("./") || value.starts_with("../") {
        Ok(normalize_path(&config_folder.join(path)))
    } else {
        anyhow::bail!(
            "Invalid {} '{}': must be an absolute path, or a relative path starting \
             with './' or '../' (resolved against the config folder).",
            field_name,
            value
        );
    }
}

/// Resolve the database path from config.
///
/// - If `database.path` is specified in config, validates it and returns the resolved path.
///   Absolute paths are returned as-is; relative paths starting with `./` or `../` are
///   resolved against the config folder.
/// - Otherwise returns `<config_folder>/default-db`.
pub fn resolve_database_path(config: &Config) -> anyhow::Result<PathBuf> {
    // Check if database.path is specified in config
    if let Some(db_config) = &config.database
        && let Some(path) = &db_config.path
    {
        return validate_config_path("database.path", path, &config.paths.config_folder);
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
mod tests;
