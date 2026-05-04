//! Per-patch lifecycle state machine.
//!
//! Replaces the scattered storage of patch state across `download_state.rs`
//! sidecars, the bare files in `downloads/`, and the `next_boot_patch` /
//! `last_booted_patch` / `known_bad_patches` fields of `PatchesState`.
//!
//! On-disk layout (per release):
//!   {root}/
//!     pointers.json                  # ReleasePointers
//!     patches/
//!       {N}/
//!         state.json                 # PatchState
//!         download                   # compressed bytes (Downloading/Downloaded only)
//!         dlc.vmcode                 # installed artifact (Installed only)
//!
//! state.json is the source of truth for "what state is patch N in?" and
//! survives within a release as a tombstone for `Bad` patches even after
//! their artifact files are removed. Everything under `patches/` is wiped
//! on release-version change.
//!
//! Mutations are exposed as two operations on top of the raw read/write:
//!   - `mark_bad(n, reason)` writes a Bad tombstone and deletes artifact
//!     files (sugar over `write_state` + `cleanup`).
//!   - `cleanup(n)` is state-aware: keeps the tombstone if the patch is
//!     already Bad, otherwise removes the patch directory entirely.
//!
//! Callers never pick between "delete tombstone" and "preserve tombstone";
//! the state on disk decides. See the design notes that led here in
//! shorebirdtech/shorebird#3737.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cache::disk_io;

const PATCHES_DIR: &str = "patches";
const PATCH_STATE_FILE: &str = "state.json";
const POINTERS_FILE: &str = "pointers.json";

/// Per-patch lifecycle state. Persisted at `{root}/patches/{N}/state.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum PatchState {
    /// Compressed bytes are partially on disk. Resume sends
    /// `Range: bytes={partial_size}-`.
    Downloading {
        url: String,
        hash: String,
        signature: Option<String>,
        partial_size: u64,
    },
    /// Compressed bytes are fully on disk and the size matches what we
    /// recorded after the download completed. Bytes are untrusted until
    /// install validates them (inflate + check_hash).
    Downloaded {
        url: String,
        hash: String,
        signature: Option<String>,
        size: u64,
    },
    /// `dlc.vmcode` is present; the patch is bootable.
    Installed {
        hash: String,
        signature: Option<String>,
        size: u64,
    },
    /// Tombstone. The patch will not be re-attempted within this release.
    /// Optional fields preserve what we knew about the patch for diagnostics
    /// and for the `PatchInstallFailure` event we queue.
    Bad {
        reason: BadReason,
        hash: Option<String>,
        signature: Option<String>,
        size: Option<u64>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum BadReason {
    /// Boot started but never recorded success — process crashed during boot.
    BootCrash,
    /// `inflate` failed (zstd magic / decompression error).
    InvalidPatchBytes,
    /// Inflated bytes' hash didn't match the server-claimed hash.
    InstallHashMismatch,
    /// `validate_patch_is_bootable` failed at boot time (size mismatch
    /// vs Installed.size, or signature failed in Strict mode).
    ValidationFailed,
}

/// Per-release pointers. Single document at `{root}/pointers.json`.
/// References patch numbers — the metadata for each lives in that patch's
/// `state.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ReleasePointers {
    /// Boot target on next launch. Must reference a patch in `Installed`.
    /// `None` means base release.
    pub next_boot_patch: Option<usize>,

    /// Most recent patch that successfully booted on a prior run. Used as
    /// a fallback target when `next_boot_patch` becomes invalid.
    pub last_booted_patch: Option<usize>,

    /// Boot-in-progress breadcrumb. Set at `record_boot_start`, cleared
    /// at `record_boot_success` / `record_boot_failure`. If still set on
    /// next init, treat as a crashed boot.
    pub currently_booting_patch: Option<usize>,

    /// Unix timestamp (seconds) when `currently_booting_patch` was set.
    pub boot_started_at: Option<u64>,
}

/// Per-release patch lifecycle and storage. Owns `{root}/patches/` and
/// `{root}/pointers.json`.
pub struct PatchLifecycle {
    root: PathBuf,
    pointers: ReleasePointers,
}

impl PatchLifecycle {
    /// Loads the lifecycle from `root`. Missing or unparseable
    /// `pointers.json` falls back to defaults; per-patch state files are
    /// read lazily.
    pub fn load_or_default(root: PathBuf) -> Self {
        let pointers_path = root.join(POINTERS_FILE);
        let pointers = if pointers_path.exists() {
            match disk_io::read(&pointers_path) {
                Ok(p) => p,
                Err(e) => {
                    shorebird_error!(
                        "Failed to read pointers from {:?}: {:?}; using defaults",
                        pointers_path,
                        e
                    );
                    ReleasePointers::default()
                }
            }
        } else {
            ReleasePointers::default()
        };
        Self { root, pointers }
    }

    pub fn pointers(&self) -> &ReleasePointers {
        &self.pointers
    }

    /// Returns the on-disk state for patch `n`, or `None` if the patch has
    /// no record on disk (i.e. is in the conceptual "Unknown" state).
    pub fn read_state(&self, n: usize) -> Option<PatchState> {
        let path = self.state_path(n);
        if !path.exists() {
            return None;
        }
        match disk_io::read(&path) {
            Ok(state) => Some(state),
            Err(e) => {
                shorebird_error!("Failed to read state for patch {}: {:?}", n, e);
                None
            }
        }
    }

    /// Persists `state` for patch `n`. Creates the patch directory if
    /// needed. Atomic via `disk_io::write`.
    pub fn write_state(&self, n: usize, state: &PatchState) -> Result<()> {
        disk_io::write(state, &self.state_path(n))
    }

    /// Persists the current pointers.
    pub fn save_pointers(&self) -> Result<()> {
        disk_io::write(&self.pointers, &self.pointers_path())
    }

    /// Transitions patch `n` to `Bad{reason}`, preserving any prior
    /// hash/signature/size info as best-effort diagnostics. Then deletes
    /// the patch's artifact files (state.json stays as the tombstone).
    ///
    /// Write-then-cleanup ordering means a crash between the two leaves a
    /// tombstone with stale-but-unused artifact bytes — sweeping picks
    /// them up on the next `cleanup` call.
    pub fn mark_bad(&self, n: usize, reason: BadReason) -> Result<()> {
        let (hash, signature, size) = match self.read_state(n) {
            Some(PatchState::Downloading {
                hash,
                signature,
                partial_size,
                ..
            }) => (Some(hash), signature, Some(partial_size)),
            Some(PatchState::Downloaded {
                hash,
                signature,
                size,
                ..
            }) => (Some(hash), signature, Some(size)),
            Some(PatchState::Installed {
                hash,
                signature,
                size,
            }) => (Some(hash), signature, Some(size)),
            Some(PatchState::Bad {
                hash,
                signature,
                size,
                ..
            }) => (hash, signature, size),
            None => (None, None, None),
        };
        self.write_state(
            n,
            &PatchState::Bad {
                reason,
                hash,
                signature,
                size,
            },
        )?;
        self.cleanup(n)
    }

    /// State-aware retirement. If patch `n` is in `Bad`, the tombstone is
    /// preserved and only artifact files are removed. Otherwise the entire
    /// patch directory is removed. Idempotent — safe to call on patches
    /// that don't exist.
    pub fn cleanup(&self, n: usize) -> Result<()> {
        match self.read_state(n) {
            Some(PatchState::Bad { .. }) => self.delete_artifact_files(n),
            Some(_) | None => self.forget_dir(n),
        }
    }

    /// Removes everything under `{root}/patches/{N}/` except `state.json`.
    fn delete_artifact_files(&self, n: usize) -> Result<()> {
        let dir = self.patch_dir(n);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return Ok(()), // Directory doesn't exist; nothing to do.
        };
        for entry in entries.flatten() {
            if entry.file_name() == PATCH_STATE_FILE {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    shorebird_error!("Failed to remove {:?}: {:?}", path, e);
                }
            } else if let Err(e) = std::fs::remove_file(&path) {
                shorebird_error!("Failed to remove {:?}: {:?}", path, e);
            }
        }
        Ok(())
    }

    /// Removes `{root}/patches/{N}/` entirely, including `state.json`.
    fn forget_dir(&self, n: usize) -> Result<()> {
        let dir = self.patch_dir(n);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    fn patches_root(&self) -> PathBuf {
        self.root.join(PATCHES_DIR)
    }

    fn patch_dir(&self, n: usize) -> PathBuf {
        self.patches_root().join(n.to_string())
    }

    fn state_path(&self, n: usize) -> PathBuf {
        self.patch_dir(n).join(PATCH_STATE_FILE)
    }

    fn pointers_path(&self) -> PathBuf {
        self.root.join(POINTERS_FILE)
    }
}

/// Convenience accessor; returns the path a caller would write the
/// compressed download bytes to. Public so the network layer can stream
/// directly into it without knowing the on-disk layout details.
pub fn download_artifact_path(root: &Path, n: usize) -> PathBuf {
    root.join(PATCHES_DIR).join(n.to_string()).join("download")
}

/// Path to the installed (inflated) artifact for patch `n`.
pub fn installed_artifact_path(root: &Path, n: usize) -> PathBuf {
    root.join(PATCHES_DIR)
        .join(n.to_string())
        .join("dlc.vmcode")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, PatchLifecycle) {
        let tmp = TempDir::new().unwrap();
        let lifecycle = PatchLifecycle::load_or_default(tmp.path().to_path_buf());
        (tmp, lifecycle)
    }

    #[test]
    fn read_state_returns_none_when_patch_unknown() {
        let (_tmp, lifecycle) = fixture();
        assert!(lifecycle.read_state(1).is_none());
    }

    #[test]
    fn write_then_read_roundtrips() {
        let (_tmp, lifecycle) = fixture();
        let state = PatchState::Downloaded {
            url: "https://example.com/p1".into(),
            hash: "abc".into(),
            signature: Some("sig".into()),
            size: 1234,
        };
        lifecycle.write_state(1, &state).unwrap();
        assert_eq!(lifecycle.read_state(1), Some(state));
    }

    #[test]
    fn read_state_is_none_for_corrupt_state_json() {
        let (_tmp, lifecycle) = fixture();
        let path = lifecycle.state_path(1);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();
        // Corrupt JSON returns None — caller treats as Unknown and starts fresh.
        assert!(lifecycle.read_state(1).is_none());
    }

    #[test]
    fn mark_bad_preserves_metadata_from_installed() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Installed {
                    hash: "h".into(),
                    signature: Some("s".into()),
                    size: 999,
                },
            )
            .unwrap();
        lifecycle.mark_bad(1, BadReason::BootCrash).unwrap();
        match lifecycle.read_state(1).unwrap() {
            PatchState::Bad {
                reason,
                hash,
                signature,
                size,
            } => {
                assert_eq!(reason, BadReason::BootCrash);
                assert_eq!(hash, Some("h".into()));
                assert_eq!(signature, Some("s".into()));
                assert_eq!(size, Some(999));
            }
            other => panic!("expected Bad, got {other:?}"),
        }
    }

    #[test]
    fn mark_bad_on_unknown_patch_records_no_metadata() {
        let (_tmp, lifecycle) = fixture();
        lifecycle.mark_bad(1, BadReason::ValidationFailed).unwrap();
        match lifecycle.read_state(1).unwrap() {
            PatchState::Bad {
                reason,
                hash,
                signature,
                size,
            } => {
                assert_eq!(reason, BadReason::ValidationFailed);
                assert!(hash.is_none());
                assert!(signature.is_none());
                assert!(size.is_none());
            }
            other => panic!("expected Bad, got {other:?}"),
        }
    }

    #[test]
    fn mark_bad_deletes_artifact_files_but_keeps_tombstone() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Downloaded {
                    url: "u".into(),
                    hash: "h".into(),
                    signature: None,
                    size: 100,
                },
            )
            .unwrap();
        // Drop fake artifact files alongside state.json.
        let dir = lifecycle.patch_dir(1);
        std::fs::write(dir.join("download"), b"compressed bytes").unwrap();
        std::fs::write(dir.join("dlc.vmcode"), b"installed bytes").unwrap();

        lifecycle.mark_bad(1, BadReason::InvalidPatchBytes).unwrap();

        assert!(lifecycle.state_path(1).exists(), "tombstone preserved");
        assert!(!dir.join("download").exists(), "artifact gone");
        assert!(!dir.join("dlc.vmcode").exists(), "artifact gone");
    }

    #[test]
    fn cleanup_on_bad_patch_keeps_tombstone() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Bad {
                    reason: BadReason::BootCrash,
                    hash: Some("h".into()),
                    signature: None,
                    size: Some(50),
                },
            )
            .unwrap();
        // Stale artifact bytes left around (e.g. from a crash between
        // mark_bad's state write and its cleanup) should be swept up.
        let dir = lifecycle.patch_dir(1);
        std::fs::write(dir.join("download"), b"stale").unwrap();

        lifecycle.cleanup(1).unwrap();

        assert!(lifecycle.state_path(1).exists());
        assert!(!dir.join("download").exists());
    }

    #[test]
    fn cleanup_on_non_bad_patch_forgets_entirely() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Installed {
                    hash: "h".into(),
                    signature: None,
                    size: 100,
                },
            )
            .unwrap();
        std::fs::write(lifecycle.patch_dir(1).join("dlc.vmcode"), b"x").unwrap();

        lifecycle.cleanup(1).unwrap();

        assert!(!lifecycle.patch_dir(1).exists());
        assert!(lifecycle.read_state(1).is_none());
    }

    #[test]
    fn cleanup_on_unknown_patch_is_noop() {
        let (_tmp, lifecycle) = fixture();
        lifecycle.cleanup(99).unwrap(); // Should not error.
    }

    #[test]
    fn cleanup_is_idempotent() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Installed {
                    hash: "h".into(),
                    signature: None,
                    size: 1,
                },
            )
            .unwrap();
        lifecycle.cleanup(1).unwrap();
        lifecycle.cleanup(1).unwrap(); // No-op the second time.
    }

    #[test]
    fn pointers_load_default_when_missing() {
        let (_tmp, lifecycle) = fixture();
        assert_eq!(lifecycle.pointers(), &ReleasePointers::default());
    }

    #[test]
    fn pointers_save_and_reload_roundtrip() {
        let tmp = TempDir::new().unwrap();
        {
            let mut lifecycle = PatchLifecycle::load_or_default(tmp.path().to_path_buf());
            lifecycle.pointers = ReleasePointers {
                next_boot_patch: Some(3),
                last_booted_patch: Some(2),
                currently_booting_patch: None,
                boot_started_at: None,
            };
            lifecycle.save_pointers().unwrap();
        }
        let reloaded = PatchLifecycle::load_or_default(tmp.path().to_path_buf());
        assert_eq!(reloaded.pointers().next_boot_patch, Some(3));
        assert_eq!(reloaded.pointers().last_booted_patch, Some(2));
    }

    #[test]
    fn pointers_load_default_on_corrupt_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(POINTERS_FILE), "not json").unwrap();
        let lifecycle = PatchLifecycle::load_or_default(tmp.path().to_path_buf());
        assert_eq!(lifecycle.pointers(), &ReleasePointers::default());
    }

    #[test]
    fn artifact_path_helpers_match_state_directory() {
        let (tmp, lifecycle) = fixture();
        let download = download_artifact_path(tmp.path(), 7);
        let installed = installed_artifact_path(tmp.path(), 7);
        assert_eq!(download.parent().unwrap(), lifecycle.patch_dir(7));
        assert_eq!(installed.parent().unwrap(), lifecycle.patch_dir(7));
    }
}
