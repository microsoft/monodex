//! Purpose: Crawl progress/time display vocabulary: timestamps for log lines, ETA strings, duration formatting.
//! Edit here when: Adding or modifying progress/time display helpers for crawl output.
//! Do not edit here for: General formatting utilities (see `app/number_format.rs`), search output (see `app/search.rs`).

/// Get current timestamp for logging (HH:MM:SS format)
/// Format current time as HH:MM:SS for progress-log use.
pub fn log_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let h = (now / 3600) % 24;
    let m = (now / 60) % 60;
    let s = now % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Format duration in seconds to human-readable string (e.g., "1h 23m" or "5m 30s")
pub fn format_duration(secs: f64) -> String {
    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let s = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else if mins > 0 {
        format!("{}m {}s", mins, s)
    } else {
        format!("{}s", s)
    }
}

/// Format ETA in seconds to human-readable string
pub fn format_eta(secs: f64) -> String {
    if secs <= 0.0 || !secs.is_finite() {
        return "--".to_string();
    }
    format_duration(secs)
}
