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

    // ==================== FileOperation Display Tests ====================

    #[test]
    fn test_operation_display_all_variants() {
        assert_eq!(format!("{}", FileOperation::CreateDir), "create directory");
        assert_eq!(format!("{}", FileOperation::CreateFile), "create file");
        assert_eq!(format!("{}", FileOperation::WriteFile), "write to file");
        assert_eq!(format!("{}", FileOperation::ReadFile), "read file");
        assert_eq!(format!("{}", FileOperation::DeleteFile), "delete file");
        assert_eq!(format!("{}", FileOperation::DeleteDir), "delete directory");
        assert_eq!(format!("{}", FileOperation::RenameFile), "rename/move file");
        assert_eq!(format!("{}", FileOperation::GetMetadata), "get file metadata");
    }

    // ==================== enhance_io_error Tests ====================

    #[test]
    fn test_enhance_io_error_includes_operation_path_and_error() {
        let error = Error::new(ErrorKind::Other, "some error");
        let path = Path::new("/some/path/file.txt");
        let message = enhance_io_error(&error, FileOperation::ReadFile, path);

        assert!(message.contains("Failed to read file"));
        assert!(message.contains("/some/path/file.txt"));
        assert!(message.contains("some error"));
    }

    #[test]
    fn test_enhance_io_error_no_hint_for_unknown_error() {
        let error = Error::new(ErrorKind::Other, "unknown error");
        let path = Path::new("/path/file.txt");
        let message = enhance_io_error(&error, FileOperation::ReadFile, path);

        // Should not contain "Possible cause" for unknown errors
        assert!(!message.contains("Possible cause"));
    }

    // ==================== Permission Denied Tests ====================

    #[test]
    fn test_permission_denied_create_dir() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/protected/dir");
        let message = enhance_io_error(&error, FileOperation::CreateDir, path);

        assert!(message.contains("Failed to create directory"));
        assert!(message.contains("write access"));
    }

    #[test]
    fn test_permission_denied_create_file() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/protected/file.txt");
        let message = enhance_io_error(&error, FileOperation::CreateFile, path);

        assert!(message.contains("Failed to create file"));
        assert!(message.contains("write access"));
    }

    #[test]
    fn test_permission_denied_write_file() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/protected/file.txt");
        let message = enhance_io_error(&error, FileOperation::WriteFile, path);

        assert!(message.contains("Failed to write to file"));
        assert!(message.contains("write access"));
    }

    #[test]
    fn test_permission_denied_read_file() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/protected/file.txt");
        let message = enhance_io_error(&error, FileOperation::ReadFile, path);

        assert!(message.contains("Failed to read file"));
        assert!(message.contains("read access"));
    }

    #[test]
    fn test_permission_denied_delete_file() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/protected/file.txt");
        let message = enhance_io_error(&error, FileOperation::DeleteFile, path);

        assert!(message.contains("Failed to delete file"));
        assert!(message.contains("permission to delete"));
    }

    #[test]
    fn test_permission_denied_delete_dir() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/protected/dir");
        let message = enhance_io_error(&error, FileOperation::DeleteDir, path);

        assert!(message.contains("Failed to delete directory"));
        assert!(message.contains("permission to delete"));
    }

    #[test]
    fn test_permission_denied_rename_file() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/protected/file.txt");
        let message = enhance_io_error(&error, FileOperation::RenameFile, path);

        assert!(message.contains("Failed to rename/move file"));
        assert!(message.contains("permission to move"));
    }

    #[test]
    fn test_permission_denied_get_metadata() {
        let error = Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let path = Path::new("/protected/file.txt");
        let message = enhance_io_error(&error, FileOperation::GetMetadata, path);

        assert!(message.contains("Failed to get file metadata"));
        assert!(message.contains("permission to access"));
        assert!(message.contains("metadata"));
    }

    // ==================== Not Found Tests ====================

    #[test]
    fn test_not_found_create_dir() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/parent/newdir");
        let message = enhance_io_error(&error, FileOperation::CreateDir, path);

        assert!(message.contains("Failed to create directory"));
        assert!(message.contains("parent directory may not exist"));
    }

    #[test]
    fn test_not_found_create_file() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/parent/file.txt");
        let message = enhance_io_error(&error, FileOperation::CreateFile, path);

        assert!(message.contains("Failed to create file"));
        assert!(message.contains("parent directory may not exist"));
    }

    #[test]
    fn test_not_found_write_file() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/file.txt");
        let message = enhance_io_error(&error, FileOperation::WriteFile, path);

        assert!(message.contains("Failed to write to file"));
        assert!(message.contains("parent directory may not exist"));
    }

    #[test]
    fn test_not_found_read_file() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/file.txt");
        let message = enhance_io_error(&error, FileOperation::ReadFile, path);

        assert!(message.contains("Failed to read file"));
        assert!(message.contains("does not exist"));
    }

    #[test]
    fn test_not_found_delete_file() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/file.txt");
        let message = enhance_io_error(&error, FileOperation::DeleteFile, path);

        assert!(message.contains("Failed to delete file"));
        assert!(message.contains("does not exist"));
    }

    #[test]
    fn test_not_found_delete_dir() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/dir");
        let message = enhance_io_error(&error, FileOperation::DeleteDir, path);

        assert!(message.contains("Failed to delete directory"));
        assert!(message.contains("does not exist"));
    }

    #[test]
    fn test_not_found_rename_file() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/file.txt");
        let message = enhance_io_error(&error, FileOperation::RenameFile, path);

        assert!(message.contains("Failed to rename/move file"));
        assert!(message.contains("source file or destination directory may not exist"));
    }

    #[test]
    fn test_not_found_get_metadata() {
        let error = Error::new(ErrorKind::NotFound, "No such file or directory");
        let path = Path::new("/nonexistent/file.txt");
        let message = enhance_io_error(&error, FileOperation::GetMetadata, path);

        assert!(message.contains("Failed to get file metadata"));
        assert!(message.contains("does not exist"));
    }

    // ==================== Other Error Kind Tests ====================

    #[test]
    fn test_already_exists_error() {
        let error = Error::new(ErrorKind::AlreadyExists, "File exists");
        let path = Path::new("/existing/file.txt");
        let message = enhance_io_error(&error, FileOperation::CreateFile, path);

        assert!(message.contains("Failed to create file"));
        assert!(message.contains("already exists"));
    }

    #[test]
    fn test_storage_full_error() {
        let error = Error::new(ErrorKind::StorageFull, "No space left on device");
        let path = Path::new("/data/file.txt");
        let message = enhance_io_error(&error, FileOperation::WriteFile, path);

        assert!(message.contains("Failed to write to file"));
        assert!(message.contains("storage is full"));
        assert!(message.contains("Free up space"));
    }

    #[test]
    fn test_read_only_filesystem_error() {
        let error = Error::new(ErrorKind::ReadOnlyFilesystem, "Read-only file system");
        let path = Path::new("/readonly/file.txt");
        let message = enhance_io_error(&error, FileOperation::WriteFile, path);

        assert!(message.contains("Failed to write to file"));
        assert!(message.contains("read-only"));
    }

    // ==================== OS Error Code Tests ====================

    #[test]
    fn test_get_os_error_hint_enospc() {
        // Test the get_os_error_hint function directly for ENOSPC (28)
        let hint = get_os_error_hint(28);
        assert!(hint.contains("ENOSPC"));
        assert!(hint.contains("storage is full"));
    }

    #[test]
    fn test_get_os_error_hint_erofs() {
        // Test the get_os_error_hint function directly for EROFS (30)
        let hint = get_os_error_hint(30);
        assert!(hint.contains("EROFS"));
        assert!(hint.contains("read-only"));
    }

    #[test]
    fn test_get_os_error_hint_edquot() {
        // Test the get_os_error_hint function directly for EDQUOT (122)
        let hint = get_os_error_hint(122);
        assert!(hint.contains("EDQUOT"));
        assert!(hint.contains("quota"));
    }

    #[test]
    fn test_get_os_error_hint_unknown_code() {
        // Unknown error codes should return empty string
        let hint = get_os_error_hint(9999);
        assert!(hint.is_empty());
    }

    #[test]
    fn test_os_error_unknown_code_in_enhance() {
        // Use an unlikely error code that won't map to a known ErrorKind
        let error = Error::from_raw_os_error(9999);
        let path = Path::new("/data/file.txt");
        let message = enhance_io_error(&error, FileOperation::WriteFile, path);

        // Should not have a "Possible cause" hint for unknown OS errors
        assert!(!message.contains("Possible cause"));
    }

    // ==================== IoResultExt Trait Tests ====================

    #[test]
    fn test_io_result_ext_ok() {
        let result: std::io::Result<i32> = Ok(42);
        let path = Path::new("/some/path");

        let converted = result.with_file_context(FileOperation::ReadFile, path);

        assert!(converted.is_ok());
        assert_eq!(converted.unwrap(), 42);
    }

    #[test]
    fn test_io_result_ext_err() {
        let result: std::io::Result<i32> =
            Err(Error::new(ErrorKind::PermissionDenied, "Permission denied"));
        let path = Path::new("/protected/file.txt");

        let converted = result.with_file_context(FileOperation::ReadFile, path);

        assert!(converted.is_err());
        let err_string = converted.unwrap_err().to_string();
        assert!(err_string.contains("Failed to read file"));
        assert!(err_string.contains("/protected/file.txt"));
    }

    #[test]
    fn test_io_result_ext_preserves_error_chain() {
        let result: std::io::Result<i32> =
            Err(Error::new(ErrorKind::NotFound, "No such file"));
        let path = Path::new("/missing/file.txt");

        let converted = result.with_file_context(FileOperation::ReadFile, path);
        let err = converted.unwrap_err();

        // The error chain should contain both the enhanced message and the original error
        let err_string = format!("{:?}", err);
        assert!(err_string.contains("No such file"));
        assert!(err_string.contains("Failed to read file"));
    }

    // ==================== FileOperation Debug/Clone Tests ====================

    #[test]
    fn test_file_operation_debug() {
        assert_eq!(format!("{:?}", FileOperation::CreateDir), "CreateDir");
        assert_eq!(format!("{:?}", FileOperation::ReadFile), "ReadFile");
    }

    #[test]
    fn test_file_operation_clone() {
        let op = FileOperation::WriteFile;
        let cloned = op;
        assert_eq!(format!("{}", op), format!("{}", cloned));
    }

    #[test]
    fn test_file_operation_copy() {
        let op1 = FileOperation::DeleteDir;
        let op2 = op1; // Copy
        assert_eq!(format!("{}", op1), format!("{}", op2));
    }
}
