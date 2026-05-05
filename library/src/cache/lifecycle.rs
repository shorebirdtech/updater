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

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{disk_io, signing};
use crate::yaml::PatchVerificationMode;

const PATCHES_DIR: &str = "patches";
const PATCH_STATE_FILE: &str = "state.json";
const POINTERS_FILE: &str = "pointers.json";

/// Per-patch lifecycle state. Persisted at `{root}/patches/{N}/state.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum PatchState {
    /// Compressed bytes are partially on disk. The current bytes-on-disk
    /// count is read from the `download` file at resume time — the state
    /// itself just records "we're mid-download for this url+hash."
    Downloading {
        url: String,
        hash: String,
        signature: Option<String>,
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
#[derive(Debug)]
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
    ///
    /// Marking an already-Bad patch overwrites the `reason` field with
    /// the new one (the old hash/signature/size are preserved). In
    /// practice we don't double-fail patches — this just makes the
    /// behavior obvious if it ever happens.
    pub fn mark_bad(&self, n: usize, reason: BadReason) -> Result<()> {
        let (hash, signature, size) = match self.read_state(n) {
            Some(PatchState::Downloading {
                hash, signature, ..
            }) => {
                let size = self
                    .download_artifact_path(n)
                    .metadata()
                    .ok()
                    .map(|m| m.len());
                (Some(hash), signature, size)
            }
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

    /// Path the caller streams compressed download bytes to. Lives at
    /// `{root}/patches/{N}/download`.
    pub fn download_artifact_path(&self, n: usize) -> PathBuf {
        self.patch_dir(n).join("download")
    }

    /// Path of the installed (inflated) artifact. Lives at
    /// `{root}/patches/{N}/dlc.vmcode`.
    pub fn installed_artifact_path(&self, n: usize) -> PathBuf {
        self.patch_dir(n).join("dlc.vmcode")
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

/// What `update_internal` should do when starting work on a patch.
///
/// Returned by [`PatchLifecycle::decide_start`] after inspecting the
/// patch's current on-disk state. The caller uses this to decide whether
/// to send a fresh GET, a Range GET, skip the network entirely, or bail.
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadAction {
    /// No usable prior bytes — start a fresh download. The caller should
    /// `record_download_started(...)` and issue a GET without a Range
    /// header.
    Fresh,
    /// Partial bytes from a matching prior attempt are on disk. The
    /// caller resumes from `offset` (the existing partial file size)
    /// and issues a GET with `Range: bytes={offset}-`.
    Resume { offset: u64 },
    /// Bytes for this exact url+hash are fully on disk. Skip the network
    /// request entirely and proceed to install.
    Complete,
    /// The patch is in a terminal state and shouldn't be re-fetched.
    Skip(SkipReason),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// Already installed; `should_install_patch` returns NoUpdate to
    /// avoid downloading the patch we're already running.
    AlreadyInstalled,
    /// Tombstoned in this release. Subsequent attempts short-circuit.
    KnownBad,
}

impl PatchLifecycle {
    /// Decide what to do when the server offers patch `n`. Reads the
    /// on-disk state and matches it against the server's `url` + `hash`.
    /// A mismatch on either field discards the prior state — the patch
    /// was deleted and re-uploaded with the same number, or routed
    /// through a different CDN URL, and the prior bytes can't be
    /// trusted.
    pub fn decide_start(&self, n: usize, url: &str, hash: &str) -> DownloadAction {
        // For Downloading/Downloaded, the on-disk file is the source
        // of truth for "how many bytes do we have." The state itself
        // just records the url/hash/signature so we can detect a
        // server change since the prior attempt.
        let download_path = self.download_artifact_path(n);
        match self.read_state(n) {
            None => DownloadAction::Fresh,
            Some(PatchState::Downloading {
                url: prior_url,
                hash: prior_hash,
                ..
            }) if prior_url == url && prior_hash == hash => {
                match std::fs::metadata(&download_path) {
                    Ok(meta) => DownloadAction::Resume { offset: meta.len() },
                    Err(_) => DownloadAction::Fresh,
                }
            }
            Some(PatchState::Downloading { .. }) => DownloadAction::Fresh,
            Some(PatchState::Downloaded {
                url: prior_url,
                hash: prior_hash,
                ..
            }) if prior_url == url && prior_hash == hash => {
                if download_path.exists() {
                    DownloadAction::Complete
                } else {
                    DownloadAction::Fresh
                }
            }
            Some(PatchState::Downloaded { .. }) => DownloadAction::Fresh,
            Some(PatchState::Installed { .. }) => {
                DownloadAction::Skip(SkipReason::AlreadyInstalled)
            }
            Some(PatchState::Bad { .. }) => DownloadAction::Skip(SkipReason::KnownBad),
        }
    }

    /// Records that a download is starting (or restarting). The actual
    /// bytes-on-disk count comes from the `download` file at resume
    /// time; this just persists the url/hash/signature so a subsequent
    /// `decide_start` can match against the server's current offer.
    pub fn record_download_started(
        &self,
        n: usize,
        url: &str,
        hash: &str,
        signature: Option<&str>,
    ) -> Result<()> {
        self.write_state(
            n,
            &PatchState::Downloading {
                url: url.to_string(),
                hash: hash.to_string(),
                signature: signature.map(String::from),
            },
        )
    }

    /// Transitions `n` from `Downloading` to `Downloaded` after the
    /// download completes. `size` is the actual on-disk size of the
    /// compressed bytes.
    pub fn record_download_complete(&self, n: usize, size: u64) -> Result<()> {
        let (url, hash, signature) = match self.read_state(n) {
            Some(PatchState::Downloading {
                url,
                hash,
                signature,
                ..
            }) => (url, hash, signature),
            // Idempotent: a second "complete" call on an already-Downloaded
            // patch is a no-op (e.g. process restarted just before this).
            Some(PatchState::Downloaded {
                url,
                hash,
                signature,
                ..
            }) => (url, hash, signature),
            other => {
                anyhow::bail!(
                    "record_download_complete called on patch {n} in unexpected state: {other:?}"
                );
            }
        };
        self.write_state(
            n,
            &PatchState::Downloaded {
                url,
                hash,
                signature,
                size,
            },
        )
    }

    /// Records that this process is starting to boot patch `n`. The
    /// breadcrumb in `pointers.currently_booting_patch` survives a process
    /// crash, which is how we detect boot-time crashes on the next init
    /// (see [`detect_boot_crash_on_init`]).
    pub fn record_boot_start(&mut self, n: usize) -> Result<()> {
        match self.read_state(n) {
            Some(PatchState::Installed { .. }) => {}
            other => {
                bail!("record_boot_start({n}) expected Installed, got {other:?}");
            }
        }
        self.pointers.currently_booting_patch = Some(n);
        self.pointers.boot_started_at = Some(crate::time::unix_timestamp());
        self.save_pointers()
    }

    /// Records a successful boot. Promotes `currently_booting_patch` to
    /// `last_booted_patch` and runs cleanup on older patches (per-patch
    /// state-aware: Bad tombstones survive, others are forgotten).
    pub fn record_boot_success(&mut self) -> Result<()> {
        let n = self
            .pointers
            .currently_booting_patch
            .context("record_boot_success without currently_booting_patch")?;
        self.pointers.last_booted_patch = Some(n);
        self.pointers.currently_booting_patch = None;
        self.pointers.boot_started_at = None;
        self.save_pointers()?;
        self.cleanup_older_than(n);
        Ok(())
    }

    /// Records that patch `n` failed to boot. Clears the boot
    /// breadcrumb, marks the patch `Bad{BootCrash}`, and recomputes
    /// `next_boot_patch`.
    ///
    /// The patch number is passed in (rather than read from
    /// `currently_booting_patch`) to match the prior PatchManager API
    /// shape — most call sites already have the number in hand. The
    /// breadcrumb is cleared regardless of whether it matched.
    pub fn record_boot_failure(&mut self, n: usize) -> Result<()> {
        self.pointers.currently_booting_patch = None;
        self.pointers.boot_started_at = None;
        self.save_pointers()?;
        self.mark_bad(n, BadReason::BootCrash)?;
        self.recompute_next_boot()
    }

    /// Called at init. If `currently_booting_patch` is still set from a
    /// prior process, that boot crashed without recording success or
    /// failure — transition the patch to `Bad{BootCrash}` and recompute
    /// `next_boot_patch`. Returns the patch number that was recovered,
    /// if any.
    pub fn detect_boot_crash_on_init(&mut self) -> Result<Option<usize>> {
        let Some(n) = self.pointers.currently_booting_patch else {
            return Ok(None);
        };
        self.record_boot_failure(n)?;
        Ok(Some(n))
    }

    /// Validates that `next_boot_patch` is bootable (its on-disk size
    /// matches `Installed.size`, and in `Strict` mode its signature
    /// verifies against `public_key`). On failure, marks the patch
    /// `Bad{ValidationFailed}` and recomputes `next_boot_patch`.
    pub fn validate_next_boot_patch(
        &mut self,
        public_key: Option<&str>,
        mode: PatchVerificationMode,
    ) -> Result<()> {
        let Some(n) = self.pointers.next_boot_patch else {
            return Ok(());
        };
        if let Err(e) = self.validate_installed_patch(n, public_key, mode) {
            shorebird_error!("Patch {} failed validation: {:?}", n, e);
            self.mark_bad(n, BadReason::ValidationFailed)?;
            self.recompute_next_boot()?;
            return Err(e);
        }
        Ok(())
    }

    /// Ensures `next_boot_patch` points at a usable Installed patch.
    /// If it already does, no-op. Otherwise (None, Bad, or Unknown) it
    /// falls back to `last_booted_patch` if that patch is currently
    /// Installed; otherwise None (boot the base release).
    ///
    /// Crucially, this does not stomp a valid `next_boot_patch` —
    /// otherwise a check that processes server rollbacks would clobber
    /// a freshly installed newer patch by promoting the older
    /// `last_booted_patch` back into `next_boot_patch`.
    ///
    /// Also clears `last_booted_patch` if its on-disk record is gone
    /// (Unknown), so `pointers.json` doesn't accumulate references to
    /// nothing. A `last_booted_patch` whose state is `Bad` is left
    /// alone — that's a useful historical breadcrumb and recompute
    /// will simply not promote it.
    ///
    /// We deliberately don't scan `patches/` for arbitrary Installed
    /// patches — within a release there are at most a couple of patches
    /// active at once, and the last successfully booted patch is the
    /// only one we have evidence works on this device.
    pub fn recompute_next_boot(&mut self) -> Result<()> {
        let mut dirty = false;
        if let Some(lb) = self.pointers.last_booted_patch {
            if self.read_state(lb).is_none() {
                self.pointers.last_booted_patch = None;
                dirty = true;
            }
        }
        let already_valid = self
            .pointers
            .next_boot_patch
            .is_some_and(|n| matches!(self.read_state(n), Some(PatchState::Installed { .. })));
        if !already_valid {
            let new_target = self
                .pointers
                .last_booted_patch
                .filter(|&lb| matches!(self.read_state(lb), Some(PatchState::Installed { .. })));
            if self.pointers.next_boot_patch != new_target {
                self.pointers.next_boot_patch = new_target;
                dirty = true;
            }
        }
        if dirty {
            self.save_pointers()?;
        }
        Ok(())
    }

    /// Sets `next_boot_patch` to a freshly Installed patch. Replaces any
    /// prior `next_boot_patch` that was Installed-but-never-booted (those
    /// are forgotten via [`cleanup`]); a Bad tombstone in that slot is
    /// preserved.
    pub fn promote_to_next_boot(&mut self, n: usize) -> Result<()> {
        if !matches!(self.read_state(n), Some(PatchState::Installed { .. })) {
            bail!("promote_to_next_boot({n}) requires Installed state");
        }
        // If we're replacing an Installed-but-never-booted previous
        // next_boot, retire it. cleanup handles tombstones correctly.
        let last_booted = self.pointers.last_booted_patch;
        if let Some(prev) = self.pointers.next_boot_patch {
            if prev != n && Some(prev) != last_booted {
                self.cleanup(prev)?;
            }
        }
        self.pointers.next_boot_patch = Some(n);
        self.save_pointers()
    }

    /// Validates a specific Installed patch against its on-disk artifact.
    fn validate_installed_patch(
        &self,
        n: usize,
        public_key: Option<&str>,
        mode: PatchVerificationMode,
    ) -> Result<()> {
        let (expected_size, signature) = match self.read_state(n) {
            Some(PatchState::Installed {
                size, signature, ..
            }) => (size, signature),
            other => bail!("Patch {n} is not Installed: {other:?}"),
        };
        let path = self.installed_artifact_path(n);
        if !path.exists() {
            bail!("Patch {n} artifact missing at {}", path.display());
        }
        let actual_size = std::fs::metadata(&path)?.len();
        if actual_size != expected_size {
            bail!(
                "Patch {n} size {} on disk, expected {}",
                actual_size,
                expected_size
            );
        }
        if mode == PatchVerificationMode::Strict {
            if let Some(public_key) = public_key {
                let signature = signature.context("Patch signature is missing")?;
                let actual_hash = signing::hash_file(&path)?;
                signing::check_signature(&actual_hash, &signature, public_key)?;
            } else {
                shorebird_info!("No public key configured; skipping signature verification");
            }
        }
        Ok(())
    }

    /// Walks `patches/` and runs [`cleanup`] on every patch with number
    /// < `n`. State-aware per-patch: Bad tombstones survive, everything
    /// else is forgotten. Best-effort — read errors are logged and
    /// skipped so a single bad entry can't block the cleanup of others.
    fn cleanup_older_than(&self, n: usize) {
        let entries = match std::fs::read_dir(self.patches_root()) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(num) = name.parse::<usize>() else {
                continue;
            };
            if num < n {
                if let Err(e) = self.cleanup(num) {
                    shorebird_error!("cleanup({}) failed: {:?}", num, e);
                }
            }
        }
    }

    /// Transitions `n` from `Downloaded` to `Installed`. `installed_size`
    /// is the on-disk size of the inflated artifact (what
    /// `validate_installed_patch` will check against on next boot).
    /// Also removes the now-unneeded compressed `download` file.
    pub fn record_install_complete(&self, n: usize, installed_size: u64) -> Result<()> {
        let (hash, signature) = match self.read_state(n) {
            Some(PatchState::Downloaded {
                hash, signature, ..
            }) => (hash, signature),
            other => {
                anyhow::bail!(
                    "record_install_complete called on patch {n} in unexpected state: {other:?}"
                );
            }
        };
        self.write_state(
            n,
            &PatchState::Installed {
                hash,
                signature,
                size: installed_size,
            },
        )?;
        // The compressed bytes are no longer needed; the dlc.vmcode is
        // the canonical artifact going forward.
        let download = self.patch_dir(n).join("download");
        if download.exists() {
            if let Err(e) = std::fs::remove_file(&download) {
                shorebird_error!("Failed to remove download file for patch {}: {:?}", n, e);
            }
        }
        Ok(())
    }
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
        let (_tmp, lifecycle) = fixture();
        let download = lifecycle.download_artifact_path(7);
        let installed = lifecycle.installed_artifact_path(7);
        assert_eq!(download.parent().unwrap(), lifecycle.patch_dir(7));
        assert_eq!(installed.parent().unwrap(), lifecycle.patch_dir(7));
    }

    #[test]
    fn decide_start_unknown_patch_is_fresh() {
        let (_tmp, lifecycle) = fixture();
        assert_eq!(
            lifecycle.decide_start(1, "https://example/p", "h"),
            DownloadAction::Fresh
        );
    }

    #[test]
    fn decide_start_resumes_matching_downloading() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Downloading {
                    url: "https://example/p".into(),
                    hash: "h".into(),
                    signature: None,
                },
            )
            .unwrap();
        std::fs::write(lifecycle.download_artifact_path(1), vec![0u8; 250]).unwrap();
        assert_eq!(
            lifecycle.decide_start(1, "https://example/p", "h"),
            DownloadAction::Resume { offset: 250 }
        );
    }

    #[test]
    fn decide_start_downloading_with_missing_file_starts_fresh() {
        let (_tmp, lifecycle) = fixture();
        // State says we were 250 bytes in, but the file is gone (e.g.
        // OS evicted it from the code cache).
        lifecycle
            .write_state(
                1,
                &PatchState::Downloading {
                    url: "https://example/p".into(),
                    hash: "h".into(),
                    signature: None,
                },
            )
            .unwrap();
        assert_eq!(
            lifecycle.decide_start(1, "https://example/p", "h"),
            DownloadAction::Fresh
        );
    }

    #[test]
    fn decide_start_url_mismatch_starts_fresh() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Downloading {
                    url: "https://old.example/p".into(),
                    hash: "h".into(),
                    signature: None,
                },
            )
            .unwrap();
        assert_eq!(
            lifecycle.decide_start(1, "https://new.example/p", "h"),
            DownloadAction::Fresh
        );
    }

    #[test]
    fn decide_start_hash_mismatch_starts_fresh() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Downloaded {
                    url: "u".into(),
                    hash: "old".into(),
                    signature: None,
                    size: 1000,
                },
            )
            .unwrap();
        assert_eq!(lifecycle.decide_start(1, "u", "new"), DownloadAction::Fresh);
    }

    #[test]
    fn decide_start_complete_skips_fetch() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Downloaded {
                    url: "u".into(),
                    hash: "h".into(),
                    signature: None,
                    size: 1000,
                },
            )
            .unwrap();
        std::fs::write(lifecycle.download_artifact_path(1), vec![0u8; 1000]).unwrap();
        assert_eq!(
            lifecycle.decide_start(1, "u", "h"),
            DownloadAction::Complete
        );
    }

    #[test]
    fn decide_start_downloaded_with_missing_file_starts_fresh() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Downloaded {
                    url: "u".into(),
                    hash: "h".into(),
                    signature: None,
                    size: 1000,
                },
            )
            .unwrap();
        assert_eq!(lifecycle.decide_start(1, "u", "h"), DownloadAction::Fresh);
    }

    #[test]
    fn decide_start_skips_installed() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Installed {
                    hash: "h".into(),
                    signature: None,
                    size: 1000,
                },
            )
            .unwrap();
        assert_eq!(
            lifecycle.decide_start(1, "u", "h"),
            DownloadAction::Skip(SkipReason::AlreadyInstalled)
        );
    }

    #[test]
    fn decide_start_skips_bad() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Bad {
                    reason: BadReason::BootCrash,
                    hash: None,
                    signature: None,
                    size: None,
                },
            )
            .unwrap();
        assert_eq!(
            lifecycle.decide_start(1, "u", "h"),
            DownloadAction::Skip(SkipReason::KnownBad)
        );
    }

    #[test]
    fn record_download_started_writes_downloading_state() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .record_download_started(1, "u", "h", Some("s"))
            .unwrap();
        assert_eq!(
            lifecycle.read_state(1).unwrap(),
            PatchState::Downloading {
                url: "u".into(),
                hash: "h".into(),
                signature: Some("s".into()),
            }
        );
    }

    #[test]
    fn record_download_complete_transitions_downloading_to_downloaded() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .record_download_started(1, "u", "h", None)
            .unwrap();
        lifecycle.record_download_complete(1, 1234).unwrap();
        assert_eq!(
            lifecycle.read_state(1).unwrap(),
            PatchState::Downloaded {
                url: "u".into(),
                hash: "h".into(),
                signature: None,
                size: 1234,
            }
        );
    }

    #[test]
    fn record_download_complete_is_idempotent_on_downloaded() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Downloaded {
                    url: "u".into(),
                    hash: "h".into(),
                    signature: None,
                    size: 1234,
                },
            )
            .unwrap();
        // Second call doesn't error; size update reflects new value (e.g.
        // a server that retried with a different chunked-encoding total).
        lifecycle.record_download_complete(1, 5678).unwrap();
        match lifecycle.read_state(1).unwrap() {
            PatchState::Downloaded { size, .. } => assert_eq!(size, 5678),
            _ => panic!("expected Downloaded"),
        }
    }

    #[test]
    fn record_download_complete_errors_on_invalid_state() {
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
        assert!(lifecycle.record_download_complete(1, 1234).is_err());
    }

    #[test]
    fn record_install_complete_transitions_to_installed_and_removes_download() {
        let (_tmp, lifecycle) = fixture();
        lifecycle
            .write_state(
                1,
                &PatchState::Downloaded {
                    url: "u".into(),
                    hash: "h".into(),
                    signature: Some("s".into()),
                    size: 1234,
                },
            )
            .unwrap();
        let download_path = lifecycle.patch_dir(1).join("download");
        std::fs::write(&download_path, b"compressed").unwrap();

        lifecycle.record_install_complete(1, 9999).unwrap();

        assert_eq!(
            lifecycle.read_state(1).unwrap(),
            PatchState::Installed {
                hash: "h".into(),
                signature: Some("s".into()),
                size: 9999,
            }
        );
        assert!(
            !download_path.exists(),
            "download file should be removed after install"
        );
    }

    #[test]
    fn record_install_complete_errors_on_invalid_state() {
        let (_tmp, lifecycle) = fixture();
        // No prior state: not Downloaded.
        assert!(lifecycle.record_install_complete(1, 1234).is_err());
    }

    fn install_patch(lifecycle: &PatchLifecycle, n: usize, size: u64) {
        let path = lifecycle.installed_artifact_path(n);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![0u8; size as usize]).unwrap();
        lifecycle
            .write_state(
                n,
                &PatchState::Installed {
                    hash: format!("hash{n}"),
                    signature: None,
                    size,
                },
            )
            .unwrap();
    }

    #[test]
    fn record_boot_start_requires_installed() {
        let (_tmp, mut lifecycle) = fixture();
        assert!(lifecycle.record_boot_start(1).is_err());

        install_patch(&lifecycle, 1, 100);
        lifecycle.record_boot_start(1).unwrap();
        assert_eq!(lifecycle.pointers().currently_booting_patch, Some(1));
        assert!(lifecycle.pointers().boot_started_at.is_some());
    }

    #[test]
    fn record_boot_success_promotes_and_cleans_older() {
        let (_tmp, mut lifecycle) = fixture();
        install_patch(&lifecycle, 1, 100);
        install_patch(&lifecycle, 2, 200);
        install_patch(&lifecycle, 3, 300);

        lifecycle.record_boot_start(3).unwrap();
        lifecycle.record_boot_success().unwrap();

        assert_eq!(lifecycle.pointers().last_booted_patch, Some(3));
        assert!(lifecycle.pointers().currently_booting_patch.is_none());
        // Older patches removed entirely.
        assert!(!lifecycle.patch_dir(1).exists());
        assert!(!lifecycle.patch_dir(2).exists());
        // Booted patch survives.
        assert!(lifecycle.patch_dir(3).exists());
    }

    #[test]
    fn record_boot_success_keeps_bad_tombstones_for_older() {
        let (_tmp, mut lifecycle) = fixture();
        install_patch(&lifecycle, 1, 100);
        install_patch(&lifecycle, 2, 200);
        install_patch(&lifecycle, 3, 300);

        // Patch 2 went bad some time ago.
        lifecycle.mark_bad(2, BadReason::BootCrash).unwrap();

        lifecycle.record_boot_start(3).unwrap();
        lifecycle.record_boot_success().unwrap();

        assert!(!lifecycle.patch_dir(1).exists(), "1 forgotten");
        // Patch 2's tombstone survives older-than cleanup.
        assert!(matches!(
            lifecycle.read_state(2),
            Some(PatchState::Bad { .. })
        ));
        assert!(lifecycle.patch_dir(3).exists());
    }

    #[test]
    fn record_boot_failure_marks_bad_and_recomputes_next_boot() {
        let (_tmp, mut lifecycle) = fixture();
        install_patch(&lifecycle, 1, 100);
        install_patch(&lifecycle, 2, 200);
        // Pretend 1 was the last-booted, 2 is queued for next boot.
        lifecycle.pointers.last_booted_patch = Some(1);
        lifecycle.pointers.next_boot_patch = Some(2);
        lifecycle.save_pointers().unwrap();

        lifecycle.record_boot_start(2).unwrap();
        lifecycle.record_boot_failure(2).unwrap();
        assert!(matches!(
            lifecycle.read_state(2),
            Some(PatchState::Bad { .. })
        ));
        // Last-booted promoted as the new next-boot.
        assert_eq!(lifecycle.pointers().next_boot_patch, Some(1));
        assert!(lifecycle.pointers().currently_booting_patch.is_none());
    }

    #[test]
    fn record_boot_failure_clears_next_boot_when_last_booted_is_also_bad() {
        let (_tmp, mut lifecycle) = fixture();
        install_patch(&lifecycle, 1, 100);
        install_patch(&lifecycle, 2, 200);
        lifecycle.mark_bad(1, BadReason::BootCrash).unwrap();
        lifecycle.pointers.last_booted_patch = Some(1);
        lifecycle.pointers.next_boot_patch = Some(2);
        lifecycle.save_pointers().unwrap();

        lifecycle.record_boot_start(2).unwrap();
        lifecycle.record_boot_failure(2).unwrap();

        // Both candidates are Bad; no fallback target → boot base.
        assert_eq!(lifecycle.pointers().next_boot_patch, None);
    }

    #[test]
    fn recompute_next_boot_clears_stale_last_booted() {
        let (_tmp, mut lifecycle) = fixture();
        // last_booted points at a patch we've forgotten — e.g. an older
        // release version was wiped and we're carrying a stale pointer.
        lifecycle.pointers.last_booted_patch = Some(7);
        lifecycle.save_pointers().unwrap();

        lifecycle.recompute_next_boot().unwrap();

        assert_eq!(lifecycle.pointers().last_booted_patch, None);
        assert_eq!(lifecycle.pointers().next_boot_patch, None);
    }

    #[test]
    fn recompute_next_boot_keeps_bad_last_booted_pointer() {
        // A `Bad` patch in last_booted is a useful breadcrumb — recompute
        // shouldn't promote it (next_boot stays None) but shouldn't clear
        // the historical pointer either.
        let (_tmp, mut lifecycle) = fixture();
        lifecycle
            .write_state(
                3,
                &PatchState::Bad {
                    reason: BadReason::BootCrash,
                    hash: None,
                    signature: None,
                    size: None,
                },
            )
            .unwrap();
        lifecycle.pointers.last_booted_patch = Some(3);
        lifecycle.save_pointers().unwrap();

        lifecycle.recompute_next_boot().unwrap();

        assert_eq!(lifecycle.pointers().last_booted_patch, Some(3));
        assert_eq!(lifecycle.pointers().next_boot_patch, None);
    }

    #[test]
    fn detect_boot_crash_on_init_recovers_when_breadcrumb_set() {
        let tmp = TempDir::new().unwrap();
        // First "process": records boot start, then "crashes" without
        // recording success or failure.
        {
            let mut lifecycle = PatchLifecycle::load_or_default(tmp.path().to_path_buf());
            install_patch(&lifecycle, 1, 100);
            lifecycle.record_boot_start(1).unwrap();
            // Drop without record_boot_success/failure.
        }
        // Second "process": init detects the breadcrumb and marks Bad.
        let mut lifecycle = PatchLifecycle::load_or_default(tmp.path().to_path_buf());
        let recovered = lifecycle.detect_boot_crash_on_init().unwrap();
        assert_eq!(recovered, Some(1));
        assert!(matches!(
            lifecycle.read_state(1),
            Some(PatchState::Bad { .. })
        ));
        assert!(lifecycle.pointers().currently_booting_patch.is_none());
    }

    #[test]
    fn detect_boot_crash_on_init_is_noop_when_no_breadcrumb() {
        let (_tmp, mut lifecycle) = fixture();
        assert_eq!(lifecycle.detect_boot_crash_on_init().unwrap(), None);
    }

    #[test]
    fn validate_next_boot_patch_marks_bad_on_size_mismatch() {
        let (_tmp, mut lifecycle) = fixture();
        // Install patch 1 with a state.json claiming size=100.
        install_patch(&lifecycle, 1, 100);
        lifecycle.pointers.next_boot_patch = Some(1);
        lifecycle.save_pointers().unwrap();
        // Truncate the artifact so it no longer matches.
        std::fs::write(lifecycle.installed_artifact_path(1), b"short").unwrap();

        let result = lifecycle.validate_next_boot_patch(None, PatchVerificationMode::default());
        assert!(result.is_err());
        assert!(matches!(
            lifecycle.read_state(1),
            Some(PatchState::Bad {
                reason: BadReason::ValidationFailed,
                ..
            })
        ));
        assert_eq!(lifecycle.pointers().next_boot_patch, None);
    }

    #[test]
    fn validate_next_boot_patch_is_noop_when_unset() {
        let (_tmp, mut lifecycle) = fixture();
        assert!(lifecycle
            .validate_next_boot_patch(None, PatchVerificationMode::default())
            .is_ok());
    }

    #[test]
    fn promote_to_next_boot_replaces_unbooted_predecessor() {
        let (_tmp, mut lifecycle) = fixture();
        install_patch(&lifecycle, 1, 100);
        install_patch(&lifecycle, 2, 200);
        lifecycle.promote_to_next_boot(1).unwrap();
        // Now install 2 and promote it; 1 was never booted (last_booted is
        // None) and should be forgotten.
        lifecycle.promote_to_next_boot(2).unwrap();
        assert_eq!(lifecycle.pointers().next_boot_patch, Some(2));
        assert!(!lifecycle.patch_dir(1).exists(), "unbooted 1 forgotten");
    }

    #[test]
    fn promote_to_next_boot_preserves_last_booted_patch() {
        let (_tmp, mut lifecycle) = fixture();
        install_patch(&lifecycle, 1, 100);
        install_patch(&lifecycle, 2, 200);
        lifecycle.pointers.last_booted_patch = Some(1);
        lifecycle.pointers.next_boot_patch = Some(1);
        lifecycle.save_pointers().unwrap();
        // Now install 2 and promote it; 1 is last_booted so survives.
        lifecycle.promote_to_next_boot(2).unwrap();
        assert_eq!(lifecycle.pointers().next_boot_patch, Some(2));
        assert!(lifecycle.patch_dir(1).exists(), "last_booted 1 preserved");
    }

    #[test]
    fn promote_to_next_boot_requires_installed() {
        let (_tmp, mut lifecycle) = fixture();
        assert!(lifecycle.promote_to_next_boot(1).is_err());
    }
}
