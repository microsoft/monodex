//! Shared utility functions

use sha2::{Digest, Sha256};
use std::hash::{Hash, Hasher};
use twox_hash::XxHash64;

/// Implementation identifier for the embedder
pub const EMBEDDER_ID: &str = "jina-embeddings-v2-base-code:v1";

/// Implementation identifier for the chunker
pub const CHUNKER_ID: &str = "typescript-partitioner:v1";

/// Compute SHA256 hash of content
pub fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    format!("sha256:{}", hex::encode(result))
}

/// Compute stable file ID from implementation identity, content, and path context.
///
/// The file ID represents a semantic version of a file - same content at the same path
/// with the same implementation produces the same ID. Path changes create new IDs
/// because breadcrumb context is semantically meaningful.
///
/// # Arguments
/// * `embedder_id` - Implementation identifier for the embedder (e.g., EMBEDDER_ID)
/// * `chunker_id` - Implementation identifier for the chunker (e.g., CHUNKER_ID)
/// * `blob_id` - Git blob SHA (content identity)
/// * `relative_path` - Path relative to catalog base (affects breadcrumb context)
pub fn compute_file_id(
    embedder_id: &str,
    chunker_id: &str,
    blob_id: &str,
    relative_path: &str,
) -> String {
    let mut hasher = XxHash64::with_seed(0);
    embedder_id.hash(&mut hasher);
    chunker_id.hash(&mut hasher);
    blob_id.hash(&mut hasher);
    relative_path.hash(&mut hasher);
    let hash = hasher.finish();
    format!("{:016x}", hash)
}

/// Compute point ID for a specific chunk within a file.
///
/// The point ID uniquely identifies a chunk by combining the file ID
/// with the chunk's ordinal position.
///
/// # Arguments
/// * `file_id` - The file's semantic identity (16-char hex string)
/// * `chunk_ordinal` - 1-indexed position of the chunk in the file
pub fn compute_point_id(file_id: &str, chunk_ordinal: usize) -> String {
    format!("{}:{}", file_id, chunk_ordinal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_id_stability() {
        let id1 = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            "abc123",
            "libraries/foo/src/index.ts",
        );
        let id2 = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            "abc123",
            "libraries/foo/src/index.ts",
        );
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_file_id_changes_with_path() {
        let id1 = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            "abc123",
            "libraries/foo/src/index.ts",
        );
        let id2 = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            "abc123",
            "libraries/bar/src/index.ts",
        );
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_file_id_changes_with_content() {
        let id1 = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            "abc123",
            "libraries/foo/src/index.ts",
        );
        let id2 = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            "def456",
            "libraries/foo/src/index.ts",
        );
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_point_id_deterministic() {
        let file_id = compute_file_id(EMBEDDER_ID, CHUNKER_ID, "abc123", "test.ts");
        let point_id_1 = compute_point_id(&file_id, 1);
        let point_id_2 = compute_point_id(&file_id, 2);

        // Different ordinals should produce different IDs
        assert_ne!(point_id_1, point_id_2);

        // Same inputs should produce same output
        assert_eq!(point_id_1, compute_point_id(&file_id, 1));
    }
}
