//! System information for deterministic embedding configuration
//!
//! This module provides memory and CPU detection for the `"auto"` embedding model
//! configuration heuristic. The heuristic is deterministic - it does not depend on
//! current system load, only on static system properties like total RAM and CPU core count.

use anyhow::Result;

/// RAM used per ONNX model instance (in bytes)
///
/// Based on empirical measurement, each ONNX session uses approximately 700MB-1GB
/// for the model weights plus runtime overhead. However, we use 2.5 GiB as the
/// planning constant to provide conservative headroom for:
/// - Memory fragmentation
/// - Peak usage during inference
/// - System memory pressure from other processes
/// - Avoiding OOM on memory-constrained systems
///
/// This conservative sizing ensures that the auto-detection heuristic errs on the
/// side of using fewer instances rather than risking OOM failures.
pub const PER_INSTANCE_RAM: u64 = 2 * 1024 * 1024 * 1024 + 512 * 1024 * 1024; // 2.5 GiB

/// Baseline RAM to reserve for OS and other processes (in bytes)
/// We reserve the larger of 4 GiB or 25% of total RAM
const BASELINE_RESERVE_MIN: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

/// Additional overhead for embedding process (tokenizer, buffers, etc.)
pub const EMBEDDING_OVERHEAD: u64 = 512 * 1024 * 1024; // 0.5 GiB

/// Resolved embedding configuration
#[derive(Debug, Clone)]
pub struct ResolvedEmbeddingConfig {
    /// Number of ONNX model instances
    pub model_instances: usize,
    /// Threads per model instance
    pub threads_per_instance: usize,
    /// Total system RAM in bytes
    pub total_ram: u64,
    /// Available RAM at startup (for warnings)
    pub available_ram: u64,
    /// Estimated RAM usage for the embedding process
    pub estimated_ram_usage: u64,
    /// Whether cgroup limits were detected (Linux only)
    pub cgroup_limited: bool,
}

/// Estimate RAM usage for a given number of model instances.
///
/// This is a simple calculation: instances × PER_INSTANCE_RAM + EMBEDDING_OVERHEAD.
/// Used for memory warnings when explicit config values are provided.
pub fn estimate_ram_usage(model_instances: usize) -> u64 {
    (model_instances as u64)
        .saturating_mul(PER_INSTANCE_RAM)
        .saturating_add(EMBEDDING_OVERHEAD)
}

/// Get the physical CPU core count, falling back to logical cores if unavailable.
///
/// This centralizes the core count logic to ensure consistency across the codebase.
pub fn get_physical_core_count() -> usize {
    // Note: We need to create a System instance to query physical core count
    // This is relatively cheap compared to System::new_all()
    use sysinfo::System;
    let sys = System::new();
    System::physical_core_count(&sys).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    })
}

/// Compute embedding configuration from system properties.
///
/// This function is deterministic - it does not depend on current system load,
/// only on static system properties like total RAM and CPU core count.
///
/// # Formula
///
/// ```text
/// effective_total_ram = min(total_memory, cgroup_memory_limit) [Linux only]
/// baseline_reserve = max(4 GiB, 25% of effective_total_ram)
/// usable_ram = effective_total_ram - baseline_reserve
/// ram_limited_instances = floor(usable_ram / PER_INSTANCE_RAM)
///
/// cpu_cap = min(4, physical_core_count)
/// modelInstances_auto = clamp(ram_limited_instances, 1, cpu_cap)
/// threadsPerInstance_auto = max(1, physical_core_count / modelInstances_auto)
/// ```
pub fn compute_auto_embedding_config() -> Result<ResolvedEmbeddingConfig> {
    use sysinfo::System;

    // Use System::new() + targeted refreshes instead of System::new_all()
    // System::new_all() enumerates all processes which is expensive and unnecessary
    let mut sys = System::new();
    sys.refresh_memory();

    // Get total system memory
    let total_memory = sys.total_memory();

    // Get CPU core count using centralized function
    let physical_cores = get_physical_core_count();

    // Calculate effective total RAM (consider cgroup limits on Linux)
    let (effective_total_ram, cgroup_limited) = get_effective_total_ram(&sys, total_memory);

    // Get available memory (for warning purposes only - not used in sizing)
    // When cgroup-limited, use cgroup's free_memory instead of host-level available
    let available_memory = if cgroup_limited {
        #[cfg(target_os = "linux")]
        {
            sys.cgroup_limits()
                .map(|cgroup_info| cgroup_info.free_memory)
                .unwrap_or_else(|| sys.available_memory())
        }
        #[cfg(not(target_os = "linux"))]
        {
            sys.available_memory()
        }
    } else {
        sys.available_memory()
    };

    // Calculate baseline reserve
    let baseline_reserve = std::cmp::max(
        BASELINE_RESERVE_MIN,
        effective_total_ram / 4, // 25%
    );

    // Calculate usable RAM for embedding
    let usable_ram = effective_total_ram.saturating_sub(baseline_reserve);

    // RAM-limited instance count
    let ram_limited_instances = if usable_ram >= PER_INSTANCE_RAM {
        (usable_ram / PER_INSTANCE_RAM) as usize
    } else {
        1 // Minimum 1 instance even with limited RAM
    };

    // CPU cap: at most 4 instances regardless of RAM
    let cpu_cap = std::cmp::min(4, physical_cores);

    // Final model instances: clamp between 1 and cpu_cap
    let model_instances = std::cmp::max(1, std::cmp::min(ram_limited_instances, cpu_cap));

    // Threads per instance: distribute remaining cores
    let threads_per_instance = std::cmp::max(1, physical_cores / model_instances);

    // Estimate RAM usage using helper function
    let estimated_ram_usage = estimate_ram_usage(model_instances);

    Ok(ResolvedEmbeddingConfig {
        model_instances,
        threads_per_instance,
        total_ram: total_memory,
        available_ram: available_memory,
        estimated_ram_usage,
        cgroup_limited,
    })
}

/// Get effective total RAM, considering cgroup limits on Linux.
///
/// On Linux, return the minimum of system RAM and cgroup memory limit.
/// On other platforms, return system RAM.
fn get_effective_total_ram(sys: &sysinfo::System, total_memory: u64) -> (u64, bool) {
    #[cfg(target_os = "linux")]
    {
        // Try to get cgroup memory limit
        if let Some(cgroup_info) = sys.cgroup_limits() {
            let cgroup_limit = cgroup_info.total_memory;
            if cgroup_limit > 0 && cgroup_limit < total_memory {
                return (cgroup_limit, true);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = sys; // Suppress unused warning
    }

    (total_memory, false)
}

/// Format bytes as human-readable string (e.g., "16.0 GB")
pub fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_auto_embedding_config() {
        let config = compute_auto_embedding_config().unwrap();

        // Basic sanity checks
        assert!(config.model_instances >= 1);
        assert!(config.model_instances <= 4);
        assert!(config.threads_per_instance >= 1);
        assert!(config.total_ram > 0);

        println!(
            "Auto config: {} instances × {} threads",
            config.model_instances, config.threads_per_instance
        );
        println!("Total RAM: {}", format_bytes(config.total_ram));
        println!("Available RAM: {}", format_bytes(config.available_ram));
        println!(
            "Estimated usage: {}",
            format_bytes(config.estimated_ram_usage)
        );
        println!("Cgroup limited: {}", config.cgroup_limited);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(1024), "1024 bytes");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
        assert_eq!(format_bytes(2_500_000_000), "2.3 GB");
    }
}
