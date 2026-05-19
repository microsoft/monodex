//! Purpose: Terminal-safe output rendering: sanitization of strings containing user-controlled bytes (`>`-prefixed lines, ANSI escapes, control characters).
//! Edit here when: Adding or modifying terminal sanitization for user-facing output.
//! Do not edit here for: Count formatting (see `app/number_format.rs`), chunk display (see `app/chunk_display.rs`).

/// E.1: Sanitize a string for safe terminal output by stripping control characters.
/// This prevents terminal injection attacks from malicious file paths, breadcrumbs, etc.
pub fn sanitize_for_terminal(s: &str) -> String {
    s.chars()
        .filter(|c| {
            // Allow printable ASCII and common Unicode, but strip control characters
            // Control characters are those with code points < 0x20 (space) and DEL (0x7F)
            // Also strip ANSI escape sequences which start with ESC (0x1B)
            !c.is_control() || *c == '\t' || *c == '\n' || *c == '\r'
        })
        .collect()
}
