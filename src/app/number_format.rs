//! Purpose: User-facing integer-count formatting (thousands separators for chunk counts, file counts, byte totals in display strings).
//! Edit here when: Adding or modifying count formatting helpers for user-facing output.
//! Do not edit here for: Progress/time formatting (see `app/crawl/progress_format.rs`), terminal sanitization (see `app/terminal_output.rs`).

/// Format an integer count with comma thousands separators.
///
/// Examples: `24396 -> "24,396"`, `4863 -> "4,863"`, `999 -> "999"`
///
/// Used for counts of things the user is processing (chunks, files, tokens, labels, warnings).
/// Not for identifiers, ordinals, fixed parameters, or values that are always small.
///
/// Std doesn't provide a thousands-separator formatter for integers in stable;
/// this is the local equivalent.
pub fn format_count(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }

    let digits: Vec<char> = n.to_string().chars().collect();
    let mut result = String::with_capacity(digits.len() + (digits.len() - 1) / 3);

    let first_group_len = digits.len() % 3;
    if first_group_len > 0 {
        for ch in &digits[..first_group_len] {
            result.push(*ch);
        }
        result.push(',');
    }

    let remaining = if first_group_len > 0 {
        &digits[first_group_len..]
    } else {
        &digits
    };

    for (i, ch) in remaining.iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(*ch);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_count_under_1000() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(1), "1");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn test_format_count_exactly_1000() {
        assert_eq!(format_count(1000), "1,000");
    }

    #[test]
    fn test_format_count_multi_comma() {
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(1000000), "1,000,000");
        assert_eq!(format_count(999999999), "999,999,999");
    }

    #[test]
    fn test_format_count_various_values() {
        assert_eq!(format_count(4863), "4,863");
        assert_eq!(format_count(24396), "24,396");
        assert_eq!(format_count(2254), "2,254");
    }
}
