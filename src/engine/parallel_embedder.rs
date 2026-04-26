//! Parallel embedding generation using multiple ONNX sessions
//!
//! This module implements a pool of ONNX sessions for parallel embedding generation.
//! Each session runs with limited intra-op threads, allowing multiple sessions to
//! run concurrently without oversubscribing CPU cores.
//!
//! Based on benchmark findings:
//! - 4 parallel sessions × 3 intra-op threads = 12 cores utilized
//! - ~12ms per embedding (3.5x faster than single session)
//! - Individual processing (no batching) is faster on CPU

use anyhow::Result;
use hf_hub::{Repo, RepoType, api::sync::Api};
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;

const MODEL_ID: &str = "jinaai/jina-embeddings-v2-base-code";
const MAX_LENGTH: usize = 8192;
const HIDDEN_SIZE: usize = 768;

/// Global cache for downloaded model files (prevents race conditions in parallel tests)
static MODEL_CACHE: Mutex<Option<(PathBuf, PathBuf)>> = Mutex::new(None);

/// Get or download the model files (thread-safe, only downloads once)
fn get_model_files() -> Result<(PathBuf, PathBuf)> {
    let mut cache = MODEL_CACHE.lock().unwrap();

    if let Some(ref paths) = *cache {
        return Ok(paths.clone());
    }

    // Download model files from HuggingFace (cached locally after first download)
    let api = Api::new()?;
    let repo = Repo::new(MODEL_ID.to_string(), RepoType::Model);
    let api = api.repo(repo);

    let tokenizer_path = api.get("tokenizer.json")?;
    let onnx_path = api.get("onnx/model.onnx")?;

    let paths = (tokenizer_path, onnx_path);
    *cache = Some(paths.clone());

    Ok(paths)
}

/// Configuration for parallel embedding
pub struct ParallelConfig {
    /// Number of worker sessions (default: 4)
    pub num_workers: usize,
    /// Threads per session for intra-op parallelism (default: 3)
    pub intra_threads: usize,
}

impl Default for ParallelConfig {
    fn default() -> Self {
        // 4 workers × 3 threads = 12 cores (good for M3 MacBook Pro)
        // Adjust based on available cores
        let total_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let num_workers = 4;
        let intra_threads = (total_cores / num_workers).max(1);

        Self {
            num_workers,
            intra_threads,
        }
    }
}

/// A pool of ONNX sessions for parallel embedding generation
pub struct ParallelEmbedder {
    // Each worker has its own session and tokenizer (both need &mut for encoding)
    workers: Vec<Arc<Mutex<(Session, Tokenizer)>>>,
}

impl ParallelEmbedder {
    /// Create a new parallel embedder with default configuration
    pub fn new() -> Result<Self> {
        Self::with_config(ParallelConfig::default())
    }

    /// Create a new parallel embedder with custom configuration
    pub fn with_config(config: ParallelConfig) -> Result<Self> {
        // Get model files (cached globally, only downloads once)
        let (tokenizer_path, onnx_path) = get_model_files()?;

        // Load base tokenizer (will be cloned for each worker)
        let base_tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        println!(
            "Creating {} ONNX sessions with {} threads each...",
            config.num_workers, config.intra_threads
        );

        // Create worker pool - each worker gets its own session AND tokenizer
        // This avoids lock contention on the tokenizer during parallel encoding
        let workers: Vec<Arc<Mutex<(Session, Tokenizer)>>> = (0..config.num_workers)
            .map(|i| {
                let session = Session::builder()
                    .expect("Failed to create session builder")
                    .with_optimization_level(GraphOptimizationLevel::All)
                    .expect("Failed to set optimization level")
                    .with_intra_threads(config.intra_threads)
                    .expect("Failed to set intra threads")
                    .commit_from_file(&onnx_path)
                    .expect("Failed to commit session");

                // Clone tokenizer for this worker
                let tokenizer = base_tokenizer.clone();

                if i == 0 {
                    println!(
                        "Worker pool created: {} workers × {} threads = {} total threads",
                        config.num_workers,
                        config.intra_threads,
                        config.num_workers * config.intra_threads
                    );
                }

                Arc::new(Mutex::new((session, tokenizer)))
            })
            .collect();

        Ok(Self { workers })
    }

    /// Get the number of workers
    pub fn num_workers(&self) -> usize {
        self.workers.len()
    }

    /// Encode a single text using a specific worker (for parallel processing)
    ///
    /// Call this from parallel iterator, passing worker_index = chunk_index % num_workers
    pub fn encode(&self, text: &str, worker_index: usize) -> Result<Vec<f32>> {
        let worker = &self.workers[worker_index % self.workers.len()];
        let mut guard = worker.lock().unwrap();
        let (session, tokenizer) = &mut *guard;

        // Tokenize with this worker's tokenizer
        let encoding = tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

        let ids = encoding.get_ids();
        let attention_mask = encoding.get_attention_mask();

        // Truncate if needed
        let seq_len = ids.len().min(MAX_LENGTH);

        // Create input tensors
        let input_ids: Vec<i64> = ids[..seq_len].iter().map(|&id| id as i64).collect();
        let attention_mask_data: Vec<i64> = attention_mask[..seq_len]
            .iter()
            .map(|&m| m as i64)
            .collect();

        // Run inference
        let outputs = session.run(ort::inputs![
            "input_ids" => Tensor::from_array(([1, seq_len], input_ids))?,
            "attention_mask" => Tensor::from_array(([1, seq_len], attention_mask_data))?,
        ])?;

        // Extract output tensor
        let (_shape, data) = outputs[0].try_extract_tensor::<f32>()?;

        // Mean pooling over sequence dimension
        let embedding: Vec<f32> = (0..HIDDEN_SIZE)
            .map(|i| (0..seq_len).map(|j| data[j * HIDDEN_SIZE + i]).sum::<f32>() / seq_len as f32)
            .collect();

        Ok(embedding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_parallel_encode() {
        let embedder = ParallelEmbedder::new().unwrap();
        let embedding = embedder.encode("function test() { return 42; }", 0);
        assert!(embedding.is_ok());
        let emb = embedding.unwrap();
        assert_eq!(emb.len(), 768);
    }

    #[test]
    fn test_parallel_performance() {
        let embedder = ParallelEmbedder::new().unwrap();
        let texts: Vec<&str> = (0..100).map(|_| "function test() { return 42; }").collect();

        use rayon::prelude::*;

        let start = Instant::now();
        let embeddings: Vec<_> = texts
            .par_iter()
            .enumerate()
            .map(|(i, text)| embedder.encode(text, i))
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let elapsed = start.elapsed();

        println!("Embedded {} chunks in {:?}", embeddings.len(), elapsed);
        println!("Per embedding: {:?}", elapsed / embeddings.len() as u32);

        assert_eq!(embeddings.len(), 100);
        for emb in &embeddings {
            assert_eq!(emb.len(), 768);
        }
    }
}
