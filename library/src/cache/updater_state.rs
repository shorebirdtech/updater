// This file deals with the cache / state management for the updater.

use std::path::{Path, PathBuf};

use anyhow::Result;
#[cfg(test)]
use anyhow::{bail, Context};
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
    /// Persistent app-storage root. Holds `state.json`, `pointers.json`,
    /// and `patches/{N}/` (state.json + dlc.vmcode). The compressed
    /// download bytes live separately under the OS-managed cache dir
    /// (passed to `load_or_new_on_error`); the lifecycle owns both
    /// roots and we don't store the download dir here directly.
    cache_dir: PathBuf,
    lifecycle: PatchLifecycle,
    patch_public_key: Option<String>,
    verification_mode: PatchVerificationMode,
    serialized_state: SerializedState,
}

/// UpdaterState fields that are serialized to disk at `{cache}/state.json`.
///
/// Every per-release field on disk (this struct, `pointers.json`, and the
/// per-patch `state.json` files under `patches/`) is wiped when
/// `release_version` changes. The release version effectively names a
/// unique build of the engine + updater, since the updater ships
/// embedded in the engine — there's no "version range" of updater code
/// that's mutually compatible. On a release-version mismatch, anything
/// we read from disk could have been written by code we don't
/// recognize, so we discard it.
#[derive(Debug, Deserialize, Serialize)]
struct SerializedState {
    /// Stable per-install ID. Survives release-version changes; only
    /// reset when the app is uninstalled. Used for analytics.
    /// <https://shorebird.dev/privacy/>
    client_id: String,
    /// The release version this cache corresponds to. Mismatch with the
    /// app's reported release version triggers a wipe of all per-release
    /// state.
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
        download_dir: PathBuf,
        release_version: String,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
        client_id: String,
    ) -> Self {
        Self {
            lifecycle: PatchLifecycle::load_or_default(cache_dir.clone(), download_dir),
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
        download_dir: &Path,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
    ) -> Result<Self> {
        let path = cache_dir.join(STATE_FILE_NAME);
        let serialized_state = disk_io::read(&path)?;
        Ok(Self {
            cache_dir: cache_dir.to_path_buf(),
            lifecycle: PatchLifecycle::load_or_default(
                cache_dir.to_path_buf(),
                download_dir.to_path_buf(),
            ),
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
        download_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
        client_id: String,
    ) -> Self {
        let mut state = Self::new(
            cache_dir.to_owned(),
            download_dir.to_owned(),
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
        // Also wipe stale compressed downloads from the prior release.
        if download_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(download_dir) {
                shorebird_error!("Failed to wipe download dir on reset: {:?}", e);
            }
        }
        // Reload lifecycle from a clean slate.
        state.lifecycle =
            PatchLifecycle::load_or_default(cache_dir.to_path_buf(), download_dir.to_path_buf());
        state
    }

    pub fn load_or_new_on_error(
        cache_dir: &Path,
        download_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
    ) -> Self {
        match Self::load(cache_dir, download_dir, patch_public_key, verification_mode) {
            Ok(loaded) => {
                if loaded.serialized_state.release_version != release_version {
                    shorebird_info!(
                        "release_version changed {} -> {}, creating new state",
                        loaded.serialized_state.release_version,
                        release_version
                    );
                    return Self::create_new_and_save(
                        cache_dir,
                        download_dir,
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
                    download_dir,
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
            &tmp.path().join("downloads"),
            release_version,
            None,
            PatchVerificationMode::default(),
        )
    }

    #[test]
    fn release_version_change_wipes_patch_state() {
        // Ports `patch_manager.rs::reset_tests::deletes_patches_dir_and_resets_patches_state`.
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
        // Ports `patch_manager.rs::add_patch_tests::adds_patch_successfully`.
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
        // Ports
        // `patch_manager.rs::next_boot_patch_tests::adding_patch_deletes_unbooted_patch_not_last_booted`.
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
        // Ports `patch_manager.rs::add_patch_tests::errs_if_file_path_does_not_exist`.
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        let bogus = PatchInfo {
            number: 1,
            path: tmp.path().join("nope"),
        };
        assert!(state.install_patch(&bogus, "h", None).is_err());
    }

    // The base64-encoded RSA key + matching signature were generated for
    // signing.rs's tests; reused here to exercise the InstallOnly path
    // without standing up our own keypair fixture.
    const TEST_PUBLIC_KEY: &str = "MIIBCgKCAQEA2wdpEGbuvlPsb9i0qYrfMefJnEw1BHTi8SYZTKrXOvJWmEpPE1hWfbkvYzXu5a96gV1yocF3DMwn04VmRlKhC4AhsD0NL0UNhYhotbKG91Kwi1vAXpHhCdz5gQEBw0K1uB4Jz+zK6WK+31PryYpwLwbyXNqXoY8IAAUQ4STsHYV5w+BMSi8pepWMRd7DR9RHcbNOZlJvdBQ5NxvB4JN4dRMq8cC73ez1P9d7Dfwv3TWY+he9EmuXLT2UivZSlHIrGBa7MFfqyUe2ro0F7Te/B0si12itBbWIqycvqcXjeOPNn6WEpqN7IWjb9LUh162JyYaz5Lb/VeeJX8LKtElccwIDAQAB";
    const TEST_HASH: &str = "404e5caa5b906f6d03c97657e8c4d604d759f9cfba1a8bba9d5b49a5ebc174f9";
    const TEST_SIGNATURE: &str = "2ixSo5LpaWUSLg2GJEV+D+uyLeLjp0c3vNXnl0yb1iJjAdpn10BFlbcwCcjaJW9PNky2HU2hKOBe62PkFHOU8DDYOfxf2LGg/ToLGPHin85WrwFAceAUYDs7JpQr43dRTbrXcT8k5tuCQOTwXecGwuWcOFFvh0GbXFnyAmi7fLfN9CtTsG2GIOle/LyYLwoviTrXn/fZTZEYrqxD/wZ4QzoWOWLWNvrPbILhqWELkBLhdZeK0+nC2CIxFRYd3bUeOi1AGtPyHKBfdwuf4VO3+HbwJVaAEiD7HU2Bj+Zp1xeSdbznmYgBV86oizrLFd23D+lBfTlmDGgdfNE9J4Z2/g==";

    fn load_with_verification(
        tmp: &TempDir,
        public_key: Option<&str>,
        mode: PatchVerificationMode,
    ) -> UpdaterState {
        UpdaterState::load_or_new_on_error(
            tmp.path(),
            &tmp.path().join("downloads"),
            "1.0.0+1",
            public_key,
            mode,
        )
    }

    #[test]
    fn install_patch_install_only_accepts_valid_signature() {
        // Ports `patch_manager.rs::add_patch_tests::install_only_succeeds_with_valid_signature`.
        let tmp = TempDir::new().unwrap();
        let mut state = load_with_verification(
            &tmp,
            Some(TEST_PUBLIC_KEY),
            PatchVerificationMode::InstallOnly,
        );
        let p = fake_artifact(&tmp, 1);
        state
            .install_patch(&p, TEST_HASH, Some(TEST_SIGNATURE))
            .unwrap();
        assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
    }

    #[test]
    fn install_patch_install_only_rejects_missing_signature() {
        // Ports
        // `patch_manager.rs::add_patch_tests::install_only_errs_if_signature_is_missing_when_public_key_configured`.
        let tmp = TempDir::new().unwrap();
        let mut state = load_with_verification(
            &tmp,
            Some(TEST_PUBLIC_KEY),
            PatchVerificationMode::InstallOnly,
        );
        let p = fake_artifact(&tmp, 1);
        assert!(state.install_patch(&p, TEST_HASH, None).is_err());
        // Failure leaves no Installed state.
        assert!(state.next_boot_patch().is_none());
    }

    #[test]
    fn install_patch_install_only_rejects_bad_signature() {
        // Ports
        // `patch_manager.rs::add_patch_tests::install_only_errs_if_signature_is_invalid`
        // and (same code path)
        // `patch_manager.rs::add_patch_tests::install_only_errs_if_public_key_is_invalid`.
        let tmp = TempDir::new().unwrap();
        let mut state = load_with_verification(
            &tmp,
            Some(TEST_PUBLIC_KEY),
            PatchVerificationMode::InstallOnly,
        );
        let p = fake_artifact(&tmp, 1);
        assert!(state
            .install_patch(&p, TEST_HASH, Some("not_a_real_signature"))
            .is_err());
    }

    #[test]
    fn boot_lifecycle_tracks_state() {
        // Ports
        // `patch_manager.rs::last_successfully_booted_patch_tests::returns_value_from_patches_state`
        // and the happy path of
        // `patch_manager.rs::record_boot_success_for_patch_tests::succeeds_when_provided_next_boot_patch_number`.
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
        // Ports
        // `patch_manager.rs::next_boot_patch_tests::returns_none_patch_if_first_patch_failed_to_boot`
        // and
        // `patch_manager.rs::record_boot_failure_for_patch_tests::deletes_failed_patch_artifacts`.
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

    #[test]
    fn install_patch_install_only_skips_verification_when_no_public_key() {
        // Ports
        // `patch_manager.rs::add_patch_tests::install_only_succeeds_with_any_signature_if_no_public_key`.
        // InstallOnly + no public_key configured → signature is never
        // checked, so any value (including garbage) is accepted.
        let tmp = TempDir::new().unwrap();
        let mut state = load_with_verification(&tmp, None, PatchVerificationMode::InstallOnly);
        let p = fake_artifact(&tmp, 1);
        state
            .install_patch(&p, "any-hash", Some("garbage-signature"))
            .unwrap();
        assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
    }
}
