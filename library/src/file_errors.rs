// This module provides enhanced error messages for file operations.
// It detects specific error types and provides more helpful context,
// especially for Android-specific issues like SELinux and Work Profiles.

use std::io::ErrorKind;
use std::path::Path;

/// Describes the type of file operation that failed.
#[derive(Debug, Clone, Copy)]
pub enum FileOperation {
    CreateDir,
    CreateFile,
    WriteFile,
    ReadFile,
    DeleteFile,
    DeleteDir,
    RenameFile,
    GetMetadata,
}

impl std::fmt::Display for FileOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileOperation::CreateDir => write!(f, "create directory"),
            FileOperation::CreateFile => write!(f, "create file"),
            FileOperation::WriteFile => write!(f, "write to file"),
            FileOperation::ReadFile => write!(f, "read file"),
            FileOperation::DeleteFile => write!(f, "delete file"),
            FileOperation::DeleteDir => write!(f, "delete directory"),
            FileOperation::RenameFile => write!(f, "rename/move file"),
            FileOperation::GetMetadata => write!(f, "get file metadata"),
        }
    }
}

/// Creates an enhanced error message for a file operation failure.
///
/// This function takes an IO error and adds context about what operation failed,
/// what path was involved, and provides hints about possible causes based on
/// the error type.
pub fn enhance_io_error(
    error: &std::io::Error,
    operation: FileOperation,
    path: &Path,
) -> String {
    let base_message = format!(
        "Failed to {} '{}': {}",
        operation,
        path.display(),
        error
    );

    let hint = get_error_hint(error, operation);

    if hint.is_empty() {
        base_message
    } else {
        format!("{}\nPossible cause: {}", base_message, hint)
    }
}

/// Returns a hint about possible causes for the given error type.
fn get_error_hint(error: &std::io::Error, operation: FileOperation) -> String {
    match error.kind() {
        ErrorKind::PermissionDenied => get_permission_denied_hint(operation),
        ErrorKind::NotFound => get_not_found_hint(operation),
        ErrorKind::AlreadyExists => {
            "A file or directory with this name already exists.".to_string()
        }
        ErrorKind::StorageFull => {
            "The device storage is full. Free up space and try again.".to_string()
        }
        ErrorKind::ReadOnlyFilesystem => {
            "The filesystem is mounted as read-only.".to_string()
        }
        _ => {
            // Check raw OS error for cases not covered by ErrorKind
            if let Some(os_error) = error.raw_os_error() {
                get_os_error_hint(os_error)
            } else {
                String::new()
            }
        }
    }
}

/// Returns hints specific to permission denied errors.
fn get_permission_denied_hint(operation: FileOperation) -> String {
    let base_hint = match operation {
        FileOperation::CreateDir | FileOperation::CreateFile | FileOperation::WriteFile => {
            "The app may not have write access to this location"
        }
        FileOperation::ReadFile => {
            "The app may not have read access to this file"
        }
        FileOperation::DeleteFile | FileOperation::DeleteDir => {
            "The app may not have permission to delete this item"
        }
        FileOperation::RenameFile => {
            "The app may not have permission to move files in this location"
        }
        FileOperation::GetMetadata => {
            "The app may not have permission to access this file's metadata"
        }
    };

    // Add Android-specific hints
    #[cfg(target_os = "android")]
    {
        format!(
            "{}. On Android, this can also be caused by: \
            (1) SELinux policy restrictions, \
            (2) the app running in a Work Profile with isolated storage, \
            (3) MDM/Knox security policies, or \
            (4) app cloning features (e.g., Dual Messenger).",
            base_hint
        )
    }

    #[cfg(not(target_os = "android"))]
    {
        base_hint.to_string()
    }
}

/// Returns hints specific to not found errors.
fn get_not_found_hint(operation: FileOperation) -> String {
    match operation {
        FileOperation::CreateDir | FileOperation::CreateFile | FileOperation::WriteFile => {
            "The parent directory may not exist.".to_string()
        }
        FileOperation::RenameFile => {
            "The source file or destination directory may not exist.".to_string()
        }
        _ => {
            "The file or directory does not exist.".to_string()
        }
    }
}

/// Returns hints for specific OS error codes not covered by ErrorKind.
fn get_os_error_hint(os_error: i32) -> String {
    // Unix/Linux error codes
    match os_error {
        28 => "The device storage is full (ENOSPC). Free up space and try again.".to_string(),
        30 => "The filesystem is mounted as read-only (EROFS).".to_string(),
        122 => "Disk quota exceeded (EDQUOT). The user's storage quota has been reached.".to_string(),
        _ => String::new(),
    }
}

/// A trait extension for adding enhanced context to IO Results.
pub trait IoResultExt<T> {
    /// Adds enhanced error context to an IO operation result.
    fn with_file_context(self, operation: FileOperation, path: &Path) -> anyhow::Result<T>;
}

impl<T> IoResultExt<T> for std::io::Result<T> {
    fn with_file_context(self, operation: FileOperation, path: &Path) -> anyhow::Result<T> {
        self.map_err(|e| {
            let enhanced_message = enhance_io_error(&e, operation, path);
            anyhow::Error::new(e).context(enhanced_message)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn test_permission_denied_message() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/data/data/com.example/cache/test.txt");
        let message = enhance_io_error(&error, FileOperation::CreateFile, path);

        assert!(message.contains("Failed to create file"));
        assert!(message.contains("/data/data/com.example/cache/test.txt"));
        assert!(message.contains("Permission denied"));
        assert!(message.contains("Possible cause"));
    }

    #[test]
    fn test_not_found_message() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/path/file.txt");
        let message = enhance_io_error(&error, FileOperation::ReadFile, path);

        assert!(message.contains("Failed to read file"));
        assert!(message.contains("does not exist"));
    }

    #[test]
    fn test_storage_full_message() {
        let error = Error::new(ErrorKind::StorageFull, "No space left on device");
        let path = Path::new("/data/file.txt");
        let message = enhance_io_error(&error, FileOperation::WriteFile, path);

        assert!(message.contains("storage is full"));
    }

    #[test]
    fn test_operation_display() {
        assert_eq!(format!("{}", FileOperation::CreateDir), "create directory");
        assert_eq!(format!("{}", FileOperation::WriteFile), "write to file");
        assert_eq!(format!("{}", FileOperation::RenameFile), "rename/move file");
    }
}
