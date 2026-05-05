// This file deals with the cache / state management for the updater.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::events::PatchEvent;
use crate::yaml::PatchVerificationMode;

use super::lifecycle::{PatchLifecycle, PatchState};
#[cfg(test)]
use super::signing;
use super::{disk_io, PatchInfo};

const STATE_FILE_NAME: &str = "state.json";

/// Records the updater's "state of the world": which patches we have
/// downloaded or installed, which patch booted last, events that need to
/// be reported to the server, etc.
///
/// Per-patch state lives inside [`PatchLifecycle`] (one document per
/// patch number under `{cache}/patches/{N}/state.json`). UpdaterState
/// itself only owns the per-device `client_id` and the per-release event
/// queue; all other patch-related fields are pointers managed by the
/// lifecycle.
// TODO(eseidel): Split the per-release state from the per-device state
// so per-device state isn't reset on release-version change.
#[derive(Debug)]
pub struct UpdaterState {
    cache_dir: PathBuf,
    lifecycle: PatchLifecycle,
    patch_public_key: Option<String>,
    verification_mode: PatchVerificationMode,
    serialized_state: SerializedState,
}

/// UpdaterState fields that are serialized to disk at `{cache}/state.json`.
#[derive(Debug, Deserialize, Serialize)]
struct SerializedState {
    /// Stable per-install ID. Survives release-version changes; only
    /// reset when the app is uninstalled. Used for analytics.
    /// <https://shorebird.dev/privacy/>
    client_id: String,
    /// The release version this cache corresponds to. If this doesn't
    /// match the release version we're booting from, the patch state
    /// is wiped and rebuilt for the new release.
    release_version: String,
    /// Events that have not yet been sent to the server. Format may
    /// change between releases, so this is per-release state.
    queued_events: Vec<PatchEvent>,
}

fn generate_client_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn is_file_not_found(error: &anyhow::Error) -> bool {
    for cause in error.chain() {
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            return io_error.kind() == std::io::ErrorKind::NotFound;
        }
    }
    false
}

impl UpdaterState {
    pub fn client_id(&self) -> String {
        self.serialized_state.client_id.clone()
    }
}

impl UpdaterState {
    fn new(
        cache_dir: PathBuf,
        release_version: String,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
        client_id: String,
    ) -> Self {
        Self {
            lifecycle: PatchLifecycle::load_or_default(cache_dir.clone()),
            cache_dir,
            patch_public_key: patch_public_key.map(|s| s.to_owned()),
            verification_mode,
            serialized_state: SerializedState {
                client_id,
                release_version,
                queued_events: Vec::new(),
            },
        }
    }

    fn load(
        cache_dir: &Path,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
    ) -> Result<Self> {
        let path = cache_dir.join(STATE_FILE_NAME);
        let serialized_state = disk_io::read(&path)?;
        Ok(Self {
            cache_dir: cache_dir.to_path_buf(),
            lifecycle: PatchLifecycle::load_or_default(cache_dir.to_path_buf()),
            patch_public_key: patch_public_key.map(|s| s.to_owned()),
            verification_mode,
            serialized_state,
        })
    }

    /// Initializes a new UpdaterState and saves it to disk. Wipes any
    /// existing per-release patch state — used when the release version
    /// changes or when the on-disk state was unparseable.
    fn create_new_and_save(
        cache_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
        client_id: String,
    ) -> Self {
        let mut state = Self::new(
            cache_dir.to_owned(),
            release_version.to_owned(),
            patch_public_key,
            verification_mode,
            client_id,
        );
        if let Err(e) = state.save() {
            shorebird_warn!("Error saving state {:?}, ignoring.", e);
        }
        // Wipe per-release patch storage from any prior release.
        let patches_root = cache_dir.join("patches");
        if patches_root.exists() {
            if let Err(e) = std::fs::remove_dir_all(&patches_root) {
                shorebird_error!("Failed to wipe patches dir on reset: {:?}", e);
            }
        }
        let pointers_path = cache_dir.join("pointers.json");
        if pointers_path.exists() {
            let _ = std::fs::remove_file(&pointers_path);
        }
        // Reload lifecycle from a clean slate.
        state.lifecycle = PatchLifecycle::load_or_default(cache_dir.to_path_buf());
        state
    }

    pub fn load_or_new_on_error(
        cache_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
    ) -> Self {
        match Self::load(cache_dir, patch_public_key, verification_mode) {
            Ok(loaded) => {
                if loaded.serialized_state.release_version != release_version {
                    shorebird_info!(
                        "release_version changed {} -> {}, creating new state",
                        loaded.serialized_state.release_version,
                        release_version
                    );
                    return Self::create_new_and_save(
                        cache_dir,
                        release_version,
                        patch_public_key,
                        verification_mode,
                        loaded.client_id(),
                    );
                }
                loaded
            }
            Err(e) => {
                if !is_file_not_found(&e) {
                    shorebird_info!("No existing state file found: {:#}, creating new state.", e);
                }
                Self::create_new_and_save(
                    cache_dir,
                    release_version,
                    patch_public_key,
                    verification_mode,
                    generate_client_id(),
                )
            }
        }
    }

    /// Saves the top-level (non-patch) state to disk.
    pub fn save(&self) -> Result<()> {
        disk_io::write(
            &self.serialized_state,
            &self.cache_dir.join(STATE_FILE_NAME),
        )
    }
}

/// Patch lifecycle accessors — UpdaterState delegates to [`PatchLifecycle`].
impl UpdaterState {
    /// Direct access to the lifecycle. Wrapping every transition in a
    /// forwarding method on UpdaterState would be churn for no reader
    /// benefit, so callers are expected to reach in for transitions
    /// (`decide_start`, `record_download_*`, `mark_bad`, etc). The
    /// boot-lifecycle / install / boot-failure helpers below are kept
    /// as wrappers because they have invariants (e.g. patch number
    /// argument validation, breadcrumb clearing) that a direct caller
    /// would have to know about.
    pub fn lifecycle(&self) -> &PatchLifecycle {
        &self.lifecycle
    }

    /// See [`lifecycle`].
    pub fn lifecycle_mut(&mut self) -> &mut PatchLifecycle {
        &mut self.lifecycle
    }

    /// Records that we are attempting to boot the patch with `patch_number`.
    pub fn record_boot_start_for_patch(&mut self, patch_number: usize) -> Result<()> {
        self.lifecycle.record_boot_start(patch_number)
    }

    /// Records that patch `patch_number` failed to boot. Marks it
    /// `Bad{BootCrash}` and recomputes `next_boot_patch`. Clears the
    /// boot breadcrumb regardless of whether it matched.
    pub fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()> {
        self.lifecycle.record_boot_failure(patch_number)
    }

    /// Records that the in-flight boot succeeded.
    pub fn record_boot_success(&mut self) -> Result<()> {
        self.lifecycle.record_boot_success()
    }

    pub fn currently_booting_patch(&self) -> Option<PatchInfo> {
        self.lifecycle
            .pointers()
            .currently_booting_patch
            .map(|n| self.patch_info(n))
    }

    pub fn boot_started_at(&self) -> Option<u64> {
        self.lifecycle.pointers().boot_started_at
    }

    pub fn last_successfully_booted_patch(&self) -> Option<PatchInfo> {
        self.lifecycle
            .pointers()
            .last_booted_patch
            .map(|n| self.patch_info(n))
    }

    /// The patch this process is using. Backed by the session-scoped
    /// global in `config.rs` — survives server-driven rollback (the
    /// running process is still using the patch) and resets on every
    /// fresh process start.
    pub fn running_patch(&self) -> Option<PatchInfo> {
        crate::config::running_patch_number().map(|n| self.patch_info(n))
    }

    pub fn set_running_patch(&mut self, patch_number: Option<usize>) {
        crate::config::set_running_patch_number(patch_number);
    }

    pub fn next_boot_patch(&mut self) -> Option<PatchInfo> {
        self.lifecycle
            .pointers()
            .next_boot_patch
            .map(|n| self.patch_info(n))
    }

    /// Validates that `next_boot_patch` is bootable. On failure, marks
    /// the patch `Bad{ValidationFailed}` and recomputes `next_boot_patch`.
    pub fn validate_next_boot_patch(&mut self) -> Result<()> {
        self.lifecycle
            .validate_next_boot_patch(self.patch_public_key.as_deref(), self.verification_mode)
    }

    /// Moves the inflated artifact at `patch.path` into the lifecycle's
    /// installed location, validates the signature in `InstallOnly`
    /// mode, transitions the patch to `Installed`, and promotes it to
    /// `next_boot_patch`.
    ///
    /// Test-only entry point. The production update flow inflates
    /// directly into the lifecycle's installed location and transitions
    /// `Downloaded → Installed` via `lifecycle::record_install_complete`,
    /// so no production caller goes through this function. Gated to
    /// `#[cfg(test)]` so a future refactor can't accidentally
    /// reintroduce the divergence — direct lifecycle calls are the
    /// canonical path. Used by `test_utils::install_fake_patch` and
    /// the tests below.
    #[cfg(test)]
    pub fn install_patch(
        &mut self,
        patch: &PatchInfo,
        hash: &str,
        signature: Option<&str>,
    ) -> Result<()> {
        if !patch.path.exists() {
            bail!("Patch file {} does not exist", patch.path.display());
        }
        // InstallOnly mode verifies the signature here; Strict mode
        // verifies it again at boot time via validate_next_boot_patch.
        if self.verification_mode == PatchVerificationMode::InstallOnly {
            if let Some(public_key) = &self.patch_public_key {
                let sig = signature.context("Patch signature is missing")?;
                signing::check_signature(hash, sig, public_key)?;
            }
        }
        let installed_path = self.lifecycle.installed_artifact_path(patch.number);
        if let Some(parent) = installed_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Defensive: if a prior partial install left a `dlc.vmcode`
        // behind, remove it before renaming so behavior is OS-agnostic
        // (POSIX `rename` overwrites silently; Windows fails).
        if installed_path.exists() {
            std::fs::remove_file(&installed_path)?;
        }
        std::fs::rename(&patch.path, &installed_path)?;
        // Mirror `record_install_complete`'s cleanup of the now-stale
        // compressed download bytes if any are sitting in the patch dir.
        let download = self.lifecycle.download_artifact_path(patch.number);
        if download.exists() {
            if let Err(e) = std::fs::remove_file(&download) {
                shorebird_error!(
                    "Failed to remove stale download for patch {}: {:?}",
                    patch.number,
                    e
                );
            }
        }
        let installed_size = std::fs::metadata(&installed_path)?.len();
        self.lifecycle.write_state(
            patch.number,
            &PatchState::Installed {
                hash: hash.to_string(),
                signature: signature.map(String::from),
                size: installed_size,
            },
        )?;
        self.lifecycle.promote_to_next_boot(patch.number)
    }

    /// Removes the artifacts for `patch_number` and recomputes pointers.
    /// Used today for server-driven rollbacks.
    pub fn uninstall_patch(&mut self, patch_number: usize) -> Result<()> {
        self.lifecycle.cleanup(patch_number)?;
        self.lifecycle.recompute_next_boot()
    }

    /// True if `patch_number` is currently in `Bad` state — we tried it
    /// and it failed, and shouldn't be retried within this release.
    pub fn is_known_bad_patch(&self, patch_number: usize) -> bool {
        matches!(
            self.lifecycle.read_state(patch_number),
            Some(PatchState::Bad { .. })
        )
    }

    fn patch_info(&self, n: usize) -> PatchInfo {
        PatchInfo {
            path: self.lifecycle.installed_artifact_path(n),
            number: n,
        }
    }
}

/// PatchEvent management.
impl UpdaterState {
    pub fn queue_event(&mut self, event: PatchEvent) -> Result<()> {
        self.serialized_state.queued_events.push(event);
        self.save()
    }

    pub fn copy_events(&self, limit: usize) -> Vec<PatchEvent> {
        self.serialized_state
            .queued_events
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn clear_events(&mut self) -> Result<()> {
        self.serialized_state.queued_events.clear();
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::lifecycle::BadReason;
    use tempfile::TempDir;

    fn fake_artifact(tmp: &TempDir, number: usize) -> PatchInfo {
        let path = tmp.path().join(format!("patch{}.full", number));
        std::fs::write(&path, format!("patch_{}_bytes", number)).unwrap();
        PatchInfo { number, path }
    }

    fn load(tmp: &TempDir, release_version: &str) -> UpdaterState {
        UpdaterState::load_or_new_on_error(
            tmp.path(),
            release_version,
            None,
            PatchVerificationMode::default(),
        )
    }

    #[test]
    fn release_version_change_wipes_patch_state() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        let p = fake_artifact(&tmp, 1);
        state.install_patch(&p, "hash", None).unwrap();
        state.save().unwrap();
        assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));

        let mut next = load(&tmp, "1.0.0+2");
        assert!(next.next_boot_patch().is_none());
    }

    #[test]
    fn client_id_persists_across_release_changes() {
        let tmp = TempDir::new().unwrap();
        let original = load(&tmp, "1.0.0+1");
        let original_client_id = original.client_id();
        let next = load(&tmp, "1.0.0+2");
        assert_eq!(next.client_id(), original_client_id);
    }

    #[test]
    fn corrupt_state_file_creates_new_state() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        let p = fake_artifact(&tmp, 1);
        state.install_patch(&p, "hash", None).unwrap();
        state.save().unwrap();

        std::fs::write(tmp.path().join(STATE_FILE_NAME), "garbage").unwrap();

        let mut reloaded = load(&tmp, "1.0.0+2");
        assert!(reloaded.next_boot_patch().is_none());
    }

    #[test]
    fn install_patch_renames_into_lifecycle_dir_and_sets_next_boot() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        let p = fake_artifact(&tmp, 1);
        state.install_patch(&p, "hash", None).unwrap();
        let next = state.next_boot_patch().unwrap();
        assert_eq!(next.number, 1);
        assert!(next.path.exists());
        assert!(!tmp.path().join("patch1.full").exists(), "source moved");
    }

    #[test]
    fn install_patch_replaces_unbooted_predecessor() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        state
            .install_patch(&fake_artifact(&tmp, 1), "h1", None)
            .unwrap();
        state
            .install_patch(&fake_artifact(&tmp, 2), "h2", None)
            .unwrap();
        assert_eq!(state.next_boot_patch().map(|p| p.number), Some(2));
        assert!(!state.lifecycle.installed_artifact_path(1).exists());
    }

    #[test]
    fn install_patch_errors_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        let bogus = PatchInfo {
            number: 1,
            path: tmp.path().join("nope"),
        };
        assert!(state.install_patch(&bogus, "h", None).is_err());
    }

    #[test]
    fn boot_lifecycle_tracks_state() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        state
            .install_patch(&fake_artifact(&tmp, 1), "h", None)
            .unwrap();
        state.record_boot_start_for_patch(1).unwrap();
        assert_eq!(state.currently_booting_patch().map(|p| p.number), Some(1));
        state.record_boot_success().unwrap();
        assert!(state.currently_booting_patch().is_none());
        assert_eq!(
            state.last_successfully_booted_patch().map(|p| p.number),
            Some(1)
        );
    }

    #[test]
    fn record_boot_failure_marks_bad_and_clears_next_boot() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        state
            .install_patch(&fake_artifact(&tmp, 1), "h", None)
            .unwrap();
        state.record_boot_start_for_patch(1).unwrap();
        state.record_boot_failure_for_patch(1).unwrap();
        assert!(state.is_known_bad_patch(1));
        assert!(state.next_boot_patch().is_none());
    }

    #[test]
    fn record_boot_failure_works_without_active_boot() {
        // Matches the prior PatchManager semantics: the call doesn't
        // require currently_booting_patch to be set; it just marks the
        // patch bad and recomputes pointers.
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        state
            .install_patch(&fake_artifact(&tmp, 1), "h", None)
            .unwrap();
        state.record_boot_failure_for_patch(1).unwrap();
        assert!(state.is_known_bad_patch(1));
        assert!(state.next_boot_patch().is_none());
    }

    #[test]
    fn uninstall_patch_clears_artifacts_and_recomputes_pointers() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        state
            .install_patch(&fake_artifact(&tmp, 1), "h", None)
            .unwrap();
        assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
        state.uninstall_patch(1).unwrap();
        assert!(state.next_boot_patch().is_none());
        assert!(!state.lifecycle.installed_artifact_path(1).exists());
    }

    #[test]
    fn is_known_bad_patch_after_mark_bad() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        state
            .install_patch(&fake_artifact(&tmp, 1), "h", None)
            .unwrap();
        state
            .lifecycle
            .mark_bad(1, BadReason::InstallHashMismatch)
            .unwrap();
        assert!(state.is_known_bad_patch(1));
    }
}
