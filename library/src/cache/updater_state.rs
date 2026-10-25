// This file deals with the cache / state management for the updater.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::events::PatchEvent;
use crate::yaml::PatchVerificationMode;

use super::lifecycle::{PatchLifecycle, PatchState};
use super::{disk_io, PatchInfo};

const STATE_FILE_NAME: &str = "state.json";

/// Files and directories under `cache_dir` that shorebird has ever
/// written. On a release-version change or unparseable state we wipe
/// these and keep going. `state.json` is intentionally absent — it's
/// rewritten in place with the preserved `client_id` (so removing it
/// would lose the only thing we want to carry forward).
///
/// `patches_state.json` is the legacy file from the prior `PatchManager`
/// implementation; carrying it forward would orphan ~few-KB of stale
/// state on every device upgrading through this PR.
// TODO(eseidel): Drop `patches_state.json` from this list two minor
// versions after the release that ships this PR. By that point the
// in-flight devices upgrading from a pre-PR build have all wiped it
// once on their first release-version change, and nothing on disk
// references it anymore.
const SHOREBIRD_OWNED_PATHS: &[&str] = &["patches", "pointers.json", "patches_state.json"];

/// Records the updater's "state of the world": which patches we have
/// downloaded or installed, which patch booted last, events that need to
/// be reported to the server, etc.
///
/// Per-patch state lives inside [`PatchLifecycle`] (one document per
/// patch number under `{cache}/patches/{N}/state.json`). UpdaterState
/// itself only owns the per-install `client_id` and the per-release event
/// queue; all other patch-related fields are pointers managed by the
/// lifecycle.
// TODO(eseidel): Split the per-release state from the per-install state
// so per-install state isn't reset on release-version change.
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

    /// Initializes a new UpdaterState and saves it to disk. Wipes the
    /// shorebird-managed files in `cache_dir` and the entire
    /// `download_dir` — used when the release version changes or when
    /// the on-disk state was unparseable.
    ///
    /// We *don't* blanket-wipe `cache_dir` because the embedder may
    /// configure it to be shared with non-shorebird files (the test
    /// suite does this; production engines typically hand us a
    /// dedicated subdir but the API doesn't enforce that). Instead we
    /// enumerate the set of files we've ever written there. Add new
    /// entries to `SHOREBIRD_OWNED_PATHS` when introducing new files
    /// or directories under `cache_dir`.
    fn create_new_and_save(
        cache_dir: &Path,
        download_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
        client_id: String,
    ) -> Self {
        for relative in SHOREBIRD_OWNED_PATHS {
            let path = cache_dir.join(relative);
            if !path.exists() {
                continue;
            }
            let result = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(e) = result {
                shorebird_error!("Failed to wipe {:?} on reset: {:?}", path, e);
            }
        }
        // The download dir is fully shorebird-owned — wipe it whole.
        if download_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(download_dir) {
                shorebird_error!("Failed to wipe download dir on reset: {:?}", e);
            }
        }

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
    use crate::test_utils::install_fake_patch_into;
    use tempfile::TempDir;

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
        install_fake_patch_into(&mut state, 1).unwrap();
        state.save().unwrap();
        assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));

        let mut next = load(&tmp, "1.0.0+2");
        assert!(next.next_boot_patch().is_none());
    }

    #[test]
    fn release_version_change_wipes_legacy_patches_state_json() {
        // Devices upgrading from the prior `PatchManager` will have a
        // `patches_state.json` left behind in cache_dir from the old
        // code. The new code never reads or writes it, but leaving it
        // on disk would orphan a few KB on every release upgrade.
        // Belongs to the SHOREBIRD_OWNED_PATHS wipe list.
        let tmp = TempDir::new().unwrap();
        let _state = load(&tmp, "1.0.0+1");
        std::fs::write(
            tmp.path().join("patches_state.json"),
            br#"{"legacy":"junk"}"#,
        )
        .unwrap();
        assert!(tmp.path().join("patches_state.json").exists());

        let _next = load(&tmp, "1.0.0+2");
        assert!(
            !tmp.path().join("patches_state.json").exists(),
            "legacy patches_state.json should be wiped on release-version change"
        );
    }

    #[test]
    fn release_version_change_wipes_download_dir() {
        // The cache-rooted download dir is per-release just like the
        // persistent patches/ tree. A release-version mismatch must
        // wipe both — otherwise an in-flight partial download from
        // the prior release could be confused for a current one.
        let tmp = TempDir::new().unwrap();
        let downloads_dir = tmp.path().join("downloads");
        std::fs::create_dir_all(&downloads_dir).unwrap();
        std::fs::write(downloads_dir.join("1"), b"stale prior-release bytes").unwrap();
        std::fs::write(downloads_dir.join("orphan"), b"junk").unwrap();

        // Release-version change triggers a full wipe.
        let _next = load(&tmp, "1.0.0+2");
        assert!(
            !downloads_dir.join("1").exists(),
            "stale prior-release download should be wiped"
        );
        assert!(
            !downloads_dir.join("orphan").exists(),
            "junk in downloads/ should be wiped"
        );
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
        install_fake_patch_into(&mut state, 1).unwrap();
        state.save().unwrap();

        std::fs::write(tmp.path().join(STATE_FILE_NAME), "garbage").unwrap();

        let mut reloaded = load(&tmp, "1.0.0+2");
        assert!(reloaded.next_boot_patch().is_none());
    }

    #[test]
    fn installing_replaces_unbooted_predecessor() {
        // Ports
        // `patch_manager.rs::next_boot_patch_tests::adding_patch_deletes_unbooted_patch_not_last_booted`.
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        install_fake_patch_into(&mut state, 1).unwrap();
        install_fake_patch_into(&mut state, 2).unwrap();
        assert_eq!(state.next_boot_patch().map(|p| p.number), Some(2));
        assert!(!state.lifecycle.installed_artifact_path(1).exists());
    }

    #[test]
    fn boot_lifecycle_tracks_state() {
        // Ports
        // `patch_manager.rs::last_successfully_booted_patch_tests::returns_value_from_patches_state`
        // and the happy path of
        // `patch_manager.rs::record_boot_success_for_patch_tests::succeeds_when_provided_next_boot_patch_number`.
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        install_fake_patch_into(&mut state, 1).unwrap();
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
        install_fake_patch_into(&mut state, 1).unwrap();
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
        install_fake_patch_into(&mut state, 1).unwrap();
        state.record_boot_failure_for_patch(1).unwrap();
        assert!(state.is_known_bad_patch(1));
        assert!(state.next_boot_patch().is_none());
    }

    #[test]
    fn uninstall_patch_clears_artifacts_and_recomputes_pointers() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        install_fake_patch_into(&mut state, 1).unwrap();
        assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
        state.uninstall_patch(1).unwrap();
        assert!(state.next_boot_patch().is_none());
        assert!(!state.lifecycle.installed_artifact_path(1).exists());
    }

    #[test]
    fn is_known_bad_patch_after_mark_bad() {
        let tmp = TempDir::new().unwrap();
        let mut state = load(&tmp, "1.0.0+1");
        install_fake_patch_into(&mut state, 1).unwrap();
        state
            .lifecycle
            .mark_bad(1, BadReason::InstallHashMismatch)
            .unwrap();
        assert!(state.is_known_bad_patch(1));
    }
}
