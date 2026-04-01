/// Tracks metadata about an in-progress patch download so that it can be
/// resumed after a failure or app restart.
///
/// Stored as a sidecar JSON file alongside the partial download:
///   {download_dir}/{patch_number}.download.json
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::file_errors::{FileOperation, IoResultExt};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DownloadState {
    /// The URL this download was started from. Used to decide whether a
    /// partial file on disk matches the current server response — if the URL
    /// changed, we discard and start fresh.
    pub url: String,
    /// The patch number being downloaded.
    pub patch_number: usize,
    /// Expected total file size from Content-Length/Content-Range (if known
    /// from a prior download attempt). Used for post-download validation.
    pub expected_size: Option<u64>,
    /// Hash of the inflated patch from the server response. Checked on resume
    /// to catch the case where a patch is deleted and re-added with the same
    /// number but different content (URL may stay the same but hash changes).
    pub expected_hash: String,
}

/// Returns the sidecar path for a given download path.
/// e.g. "{download_dir}/1" -> "{download_dir}/1.download.json"
pub fn sidecar_path(download_path: &Path) -> PathBuf {
    let mut p = download_path.as_os_str().to_owned();
    p.push(".download.json");
    PathBuf::from(p)
}

/// Write a DownloadState to its sidecar file.
pub fn write_download_state(download_path: &Path, state: &DownloadState) -> anyhow::Result<()> {
    let path = sidecar_path(download_path);
    let json = serde_json::to_string(state)?;
    std::fs::write(&path, json).with_file_context(FileOperation::WriteFile, &path)?;
    Ok(())
}

/// Read a DownloadState from its sidecar file, if it exists.
pub fn read_download_state(download_path: &Path) -> anyhow::Result<Option<DownloadState>> {
    let path = sidecar_path(download_path);
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(&path).with_file_context(FileOperation::ReadFile, &path)?;
    let state: DownloadState = serde_json::from_str(&json)?;
    Ok(Some(state))
}

/// Delete the sidecar file for a download, if it exists.
pub fn delete_download_state(download_path: &Path) -> anyhow::Result<()> {
    let path = sidecar_path(download_path);
    if path.exists() {
        std::fs::remove_file(&path).with_file_context(FileOperation::DeleteFile, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip_download_state() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("1");

        let state = DownloadState {
            url: "https://example.com/patch/1".to_string(),
            patch_number: 1,
            expected_size: Some(12345),
            expected_hash: "abc123".to_string(),
        };

        write_download_state(&download_path, &state).unwrap();
        let loaded = read_download_state(&download_path).unwrap();
        assert_eq!(loaded, Some(state));
    }

    #[test]
    fn read_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("99");
        let loaded = read_download_state(&download_path).unwrap();
        assert_eq!(loaded, None);
    }

    #[test]
    fn delete_removes_sidecar() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("1");

        let state = DownloadState {
            url: "https://example.com/patch/1".to_string(),
            patch_number: 1,
            expected_size: None,
            expected_hash: "abc".to_string(),
        };

        write_download_state(&download_path, &state).unwrap();
        assert!(sidecar_path(&download_path).exists());

        delete_download_state(&download_path).unwrap();
        assert!(!sidecar_path(&download_path).exists());
    }

    #[test]
    fn delete_missing_is_ok() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("99");
        // Should not error.
        delete_download_state(&download_path).unwrap();
    }

    #[test]
    fn sidecar_path_is_correct() {
        let p = sidecar_path(Path::new("/cache/downloads/1"));
        assert_eq!(p, PathBuf::from("/cache/downloads/1.download.json"));
    }
}
