//! Purpose: Debug logging hooks for partitioning decisions — the `PartitionDebug` flag struct and verbose-output controls.
//! Edit here when: Adding or modifying debug output for chunking decisions.
//! Do not edit here for: Types (see `types.rs`), scoring (see `scoring.rs`), split logic (see `split_search.rs`).

/// Debug logging for partitioning decisions
#[derive(Debug, Clone, Copy, Default)]
pub struct PartitionDebug {
    /// Enable verbose logging of split decisions
    pub enabled: bool,
}

impl PartitionDebug {
    pub fn log(&self, msg: &str) {
        if !self.enabled {
            return;
        }
        eprintln!("[DEBUG] {}", msg);
    }

    pub fn log_split_attempt(&self, start_line: usize, end_line: usize, chunk_size: usize) {
        if !self.enabled {
            return;
        }
        eprintln!(
            "[DEBUG] === Splitting chunk lines {}-{} ({} chars) ===",
            start_line, end_line, chunk_size
        );
    }

    pub fn log_scope(&self, scope_type: &str, kind: &str, start_line: usize, end_line: usize) {
        if !self.enabled {
            return;
        }
        eprintln!(
            "[DEBUG] {} scope '{}' at lines {}-{}",
            scope_type, kind, start_line, end_line
        );
    }

    pub fn log_candidates(&self, candidates: &[usize]) {
        if !self.enabled {
            return;
        }
        eprintln!("[DEBUG]   Candidates: {:?}", candidates);
    }

    pub fn log_split_decision(&self, result: &str, split_line: Option<usize>) {
        if !self.enabled {
            return;
        }
        match split_line {
            Some(line) => eprintln!("[DEBUG]   => {} at line {}", result, line),
            None => eprintln!("[DEBUG]   => {}", result),
        }
    }

    pub fn log_meaningful_child(&self, kind: &str, start_line: usize, end_line: usize) {
        if !self.enabled {
            return;
        }
        eprintln!(
            "[DEBUG]   Meaningful child: '{}' at lines {}-{}",
            kind, start_line, end_line
        );
    }
}
