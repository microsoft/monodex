//! Working-directory source sentinel generation.
//!
//! Purpose: Generate per-crawl-unique sentinel strings for working-directory crawls.
//! Edit here when: Changing the sentinel format or generation logic.
//! Do not edit here for: Crawl source metadata types (see app/crawl/types.rs).

use std::time::{SystemTime, UNIX_EPOCH};

/// Generate a per-crawl-unique sentinel string for working-directory mode.
///
/// Format: `"working-dir:<unix-secs>:<8-hex-random>"`
///
/// The sentinel is unique per crawl so any two working-dir crawls compare unequal.
/// This is conservative: the working tree may have changed between two crawls
/// and we cannot cheaply detect equality.
pub fn make_working_dir_source_sentinel() -> String {
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Generate 8 hex characters of randomness from a single u32
    let random_hex = format!("{:08x}", rand::random::<u32>());

    format!("working-dir:{}:{}", unix_secs, random_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sentinel_format() {
        let sentinel = make_working_dir_source_sentinel();
        assert!(sentinel.starts_with("working-dir:"));
        // Format: working-dir:<secs>:<8-hex>
        let parts: Vec<&str> = sentinel.split(':').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "working-dir");
        assert!(parts[1].parse::<u64>().is_ok(), "seconds should be numeric");
        assert_eq!(parts[2].len(), 8, "random hex should be 8 characters");
    }

    #[test]
    fn test_sentinels_are_unique() {
        let s1 = make_working_dir_source_sentinel();
        let s2 = make_working_dir_source_sentinel();
        // Extremely unlikely to be equal even if called rapidly
        assert_ne!(s1, s2);
    }
}
