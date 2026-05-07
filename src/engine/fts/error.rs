//! FTS error handling utilities.
//!
//! Purpose: Typed error discrimination for Tantivy errors.
//! Edit here when: Adding new error discrimination helpers for FTS operations.
//! Do not edit here for: General error types (see engine/storage/), CLI error handling (see app/).

use std::io;
use tantivy::TantivyError;
use tantivy::directory::error::{OpenDirectoryError, OpenReadError};

/// Returns true if the Tantivy error indicates a directory or file that does not exist.
///
/// This is the load-bearing piece of the lock-free reader contract: when a concurrent
/// `purge --catalog` removes the FTS directory after a reader has opened it, the reader
/// should gracefully return `NoIndex` rather than propagating an error.
///
/// # Arguments
/// * `err` - A reference to a TantivyError
///
/// # Returns
/// `true` if the error indicates a NotFound condition, `false` otherwise
pub(super) fn is_not_found_error(err: &TantivyError) -> bool {
    match err {
        // Direct IO error with NotFound kind
        TantivyError::IoError(io_err) => io_err.kind() == io::ErrorKind::NotFound,

        // OpenDirectoryError::DoesNotExist is the canonical "directory missing" signal
        TantivyError::OpenDirectoryError(OpenDirectoryError::DoesNotExist(_)) => true,

        // OpenReadError::FileDoesNotExist indicates a missing file
        TantivyError::OpenReadError(OpenReadError::FileDoesNotExist(_)) => true,

        // Check wrapped IO errors in OpenDirectoryError
        TantivyError::OpenDirectoryError(OpenDirectoryError::IoError { io_error, .. }) => {
            io_error.kind() == io::ErrorKind::NotFound
        }

        // Check wrapped IO errors in OpenReadError
        TantivyError::OpenReadError(OpenReadError::IoError { io_error, .. }) => {
            io_error.kind() == io::ErrorKind::NotFound
        }

        // All other errors are not "not found"
        _ => false,
    }
}

/// Returns true if the io::Error indicates a NotFound condition.
pub(super) fn is_io_not_found(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_is_not_found_io_error() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let tantivy_err = TantivyError::IoError(std::sync::Arc::new(io_err));
        assert!(is_not_found_error(&tantivy_err));
    }

    #[test]
    fn test_is_not_found_other_io_error() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "permission denied");
        let tantivy_err = TantivyError::IoError(std::sync::Arc::new(io_err));
        assert!(!is_not_found_error(&tantivy_err));
    }

    #[test]
    fn test_is_not_found_open_directory_does_not_exist() {
        let err = OpenDirectoryError::DoesNotExist(PathBuf::from("/path/to/index"));
        let tantivy_err = TantivyError::OpenDirectoryError(err);
        assert!(is_not_found_error(&tantivy_err));
    }

    #[test]
    fn test_is_not_found_open_read_file_does_not_exist() {
        let err = OpenReadError::FileDoesNotExist(PathBuf::from("/path/to/file"));
        let tantivy_err = TantivyError::OpenReadError(err);
        assert!(is_not_found_error(&tantivy_err));
    }

    #[test]
    fn test_is_not_found_other_error() {
        let tantivy_err = TantivyError::InvalidArgument("bad query".to_string());
        assert!(!is_not_found_error(&tantivy_err));
    }
}
