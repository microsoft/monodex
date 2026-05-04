//! Purpose: gix-based reading of Git commit trees — enumerate files, read blobs, build the package index for a resolved commit.
//! Edit here when: Changing how files or blobs are read from commit objects, or how the package index is built from a commit tree.
//! Do not edit here for: The `BlobSource` trait or `PackageIndex` type (see `mod.rs`), working-directory subprocess code (see `working_dir.rs`).

use anyhow::{Result, anyhow};
use gix::ObjectId;
use gix::objs::TreeRefIter;
use gix::traverse::tree::Recorder;
use std::path::Path;

use super::{FileEntry, PackageIndex, extract_package_name_from_bytes};

pub fn resolve_commit_oid(repo_path: &Path, commit: &str) -> Result<String> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    Ok(commit_id.to_hex().to_string())
}

pub fn enumerate_commit_tree(repo_path: &Path, commit: &str) -> Result<Vec<FileEntry>> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    let commit_obj = repo
        .find_object(commit_id)
        .map_err(|e| anyhow!("Failed to find commit object: {}", e))?;

    let tree_id: ObjectId = {
        let commit = commit_obj
            .try_into_commit()
            .map_err(|_| anyhow!("'{}' is not a commit", commit))?;
        commit
            .tree_id()
            .map_err(|e| anyhow!("Failed to get tree ID: {}", e))?
            .detach()
    };

    let tree_data = {
        let tree_obj = repo
            .find_object(tree_id)
            .map_err(|e| anyhow!("Failed to find tree object: {}", e))?;
        tree_obj.data.clone()
    };

    let mut recorder = Recorder::default();
    gix::traverse::tree::breadthfirst(
        TreeRefIter::from_bytes(&tree_data),
        &mut gix::traverse::tree::breadthfirst::State::default(),
        repo.objects,
        &mut recorder,
    )
    .map_err(|e| anyhow!("Failed to traverse tree: {}", e))?;

    Ok(recorder
        .records
        .into_iter()
        .filter(|entry| entry.mode.is_blob())
        .map(|entry| FileEntry {
            relative_path: entry.filepath.to_string(),
            blob_id: entry.oid.to_hex().to_string(),
        })
        .collect())
}

pub fn read_blob_content(repo_path: &Path, blob_id: &str) -> Result<Vec<u8>> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let object_id = ObjectId::from_hex(blob_id.as_bytes())
        .map_err(|e| anyhow!("Invalid blob ID '{}': {}", blob_id, e))?;

    let blob = repo
        .find_object(object_id)
        .map_err(|e| anyhow!("Failed to find blob '{}': {}", blob_id, e))?
        .try_into_blob()
        .map_err(|_| anyhow!("Object '{}' is not a blob", blob_id))?;

    Ok(blob.data.to_vec())
}

pub fn build_package_index_for_commit(repo_path: &Path, commit: &str) -> Result<PackageIndex> {
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow!("Failed to open repository at {:?}: {}", repo_path, e))?;

    let commit_id: ObjectId = repo
        .rev_parse_single(commit)
        .map_err(|e| anyhow!("Failed to resolve commit '{}': {}", commit, e))?
        .detach();

    let commit_obj = repo
        .find_object(commit_id)
        .map_err(|e| anyhow!("Failed to find commit object: {}", e))?;

    let tree_id: ObjectId = {
        let commit = commit_obj
            .try_into_commit()
            .map_err(|_| anyhow!("'{}' is not a commit", commit))?;
        commit
            .tree_id()
            .map_err(|e| anyhow!("Failed to get tree ID: {}", e))?
            .detach()
    };

    let tree_data = {
        let tree_obj = repo
            .find_object(tree_id)
            .map_err(|e| anyhow!("Failed to find tree object: {}", e))?;
        tree_obj.data.clone()
    };

    let mut recorder = Recorder::default();
    gix::traverse::tree::breadthfirst(
        TreeRefIter::from_bytes(&tree_data),
        &mut gix::traverse::tree::breadthfirst::State::default(),
        repo.objects.clone(),
        &mut recorder,
    )
    .map_err(|e| anyhow!("Failed to traverse tree: {}", e))?;

    let package_json_entries: Vec<(String, ObjectId)> = recorder
        .records
        .iter()
        .filter(|entry| entry.mode.is_blob())
        .filter_map(|entry| {
            let filepath_bytes: &[u8] = entry.filepath.as_ref();
            let filename = filepath_bytes
                .rsplit(|b| *b == b'/')
                .next()
                .unwrap_or_default();

            if filename == b"package.json" {
                let filepath_str = String::from_utf8_lossy(filepath_bytes);
                let dir_path = filepath_str
                    .strip_suffix("/package.json")
                    .or_else(|| filepath_str.strip_suffix("package.json"))
                    .unwrap_or("")
                    .to_string();
                Some((dir_path, entry.oid))
            } else {
                None
            }
        })
        .collect();

    let mut index = PackageIndex::new();
    for (dir_path, blob_id) in package_json_entries {
        if let Ok(obj) = repo.find_object(blob_id)
            && let Ok(blob) = obj.try_into_blob()
            && let Some(name) = extract_package_name_from_bytes(&blob.data)
        {
            index.package_name_by_dir.insert(dir_path, name);
        }
    }

    Ok(index)
}
