// This file deals with the cache / state management for the updater.

// This code is very confused and uses "patch number" sometimes
// and "slot index" others.  The public interface should be
// consistent and use patch number everywhere.
// PatchInfo can probably go away.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::events::PatchEvent;
use crate::yaml::PatchVerificationMode;

use super::patch_manager::{ManagePatches, PatchManager};
use super::{disk_io, PatchInfo};

/// Where the updater state is stored on disk.
const STATE_FILE_NAME: &str = "state.json";

/// Records the updater's "state of the world" - which patches we know to be
/// good or bad, which patches we have downloaded, which patch we're currently
/// booted from, events that need to be reported to the server, etc.
///
// This struct is public, as callers can have a handle to it, but modifying
// anything inside should be done via the functions below.
// TODO(eseidel): Split the per-release state from the per-device state.
// That way per-release state is reset when the release version changes.
// but per-device state is not.
#[derive(Debug)]
pub struct UpdaterState {
    // Per-device state:
    /// Where this writes to disk. Don't serialize this field, as it can change
    /// between runs of the app.
    cache_dir: PathBuf,

    patch_manager: Box<dyn ManagePatches>,

    serialized_state: SerializedState,
}

/// UpdaterState fields that are serialized to disk.
///
/// Written out to disk as a json file at STATE_FILE_NAME.
#[derive(Debug, Deserialize, Serialize)]
struct SerializedState {
    /// The client ID for this device. This is assigned on the first launch of this app and persists
    /// between release versions. This is only reset when the app is uninstalled.
    /// Shorebird uses these per-install ids in order to provide you, the customer,
    /// install-count analytics for your apps. Storage or use of this, and any other,
    /// information is covered in our privacy policy: https://shorebird.dev/privacy/
    client_id: String,
    // Per-release state:
    /// The release version this cache corresponds to.
    /// If this does not match the release version we're booting from we will
    /// clear the cache.
    release_version: String,
    /// Events that have not yet been sent to the server.
    /// Format could change between releases, so this is per-release state.
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

/// Serialized updater state
impl UpdaterState {
    pub fn client_id(&self) -> String {
        self.serialized_state.client_id.clone()
    }
}

/// Lifecycle methods for the updater state.
impl UpdaterState {
    /// Creates a new `UpdaterState`.
    fn new(
        cache_dir: PathBuf,
        release_version: String,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
        client_id: String,
    ) -> Self {
        Self {
            cache_dir: cache_dir.clone(),
            patch_manager: Box::new(PatchManager::new(
                cache_dir.clone(),
                patch_public_key,
                verification_mode,
            )),
            serialized_state: SerializedState {
                client_id: client_id,
                release_version,
                queued_events: Vec::new(),
            },
        }
    }

    /// Loads UpdaterState from disk
    fn load(
        cache_dir: &Path,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
    ) -> anyhow::Result<Self> {
        let path = cache_dir.join(STATE_FILE_NAME);
        let serialized_state = disk_io::read(&path)?;
        Ok(UpdaterState {
            cache_dir: cache_dir.to_path_buf(),
            patch_manager: Box::new(PatchManager::new(
                cache_dir.to_path_buf(),
                patch_public_key,
                verification_mode,
            )),
            serialized_state,
        })
    }

    /// Initializes a new UpdaterState and saves it to disk.
    fn create_new_and_save(
        storage_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
        client_id: String,
    ) -> Self {
        let mut state = Self::new(
            storage_dir.to_owned(),
            release_version.to_owned(),
            patch_public_key,
            verification_mode,
            client_id,
        );
        if let Err(e) = state.save() {
            shorebird_warn!("Error saving state {:?}, ignoring.", e);
        }
        // Ensure we clear any patch data if we're creating a new state.
        let _ = state.patch_manager.reset();
        state
    }

    pub fn load_or_new_on_error(
        storage_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
    ) -> Self {
        let load_result = Self::load(storage_dir, patch_public_key, verification_mode);
        match load_result {
            Ok(loaded) => {
                if loaded.serialized_state.release_version != release_version {
                    shorebird_info!(
                        "release_version changed {} -> {}, creating new state",
                        loaded.serialized_state.release_version,
                        release_version
                    );
                    return Self::create_new_and_save(
                        storage_dir,
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
                    storage_dir,
                    release_version,
                    patch_public_key,
                    verification_mode,
                    generate_client_id(),
                )
            }
        }
    }

    /// Saves the updater state to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Path::new(&self.cache_dir).join(STATE_FILE_NAME);
        disk_io::write(&self.serialized_state, &path)
    }
}

/// Patch management. All patch management is done via the patch manager.
impl UpdaterState {
    /// Records that we are attempting to boot the patch with patch_number.
    pub fn record_boot_start_for_patch(&mut self, patch_number: usize) -> Result<()> {
        self.patch_manager.record_boot_start_for_patch(patch_number)
    }

    /// Records that the patch with patch_number failed to boot, uninstalls the patch.
    pub fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()> {
        self.patch_manager
            .record_boot_failure_for_patch(patch_number)
    }

    /// Records that the patch with patch_number was successfully booted, marks the patch as "good".
    pub fn record_boot_success(&mut self) -> Result<()> {
        self.patch_manager.record_boot_success()
    }

    /// The patch that is currently in the process of booting. That is, we've recorded a boot start
    /// but not yet a boot success or failure.
    pub fn currently_booting_patch(&self) -> Option<PatchInfo> {
        self.patch_manager.currently_booting_patch()
    }

    /// The last patch that was successfully booted (e.g., for which we record_boot_success was
    /// called).
    /// Will be None if:
    /// - There was no good patch at time of boot.
    /// - The updater has been initialized but no boot recorded yet.
    pub fn last_successfully_booted_patch(&self) -> Option<PatchInfo> {
        self.patch_manager.last_successfully_booted_patch()
    }

    /// This is the current patch that is running.
    pub fn current_boot_patch(&self) -> Option<PatchInfo> {
        self.patch_manager
            .currently_booting_patch()
            .or(self.patch_manager.last_successfully_booted_patch())
    }

    /// This is the patch that will be used for the next boot.
    /// Will be None if:
    /// - There has never been a patch selected.
    /// - There was a patch selected but it was later marked as bad.
    pub fn next_boot_patch(&mut self) -> Option<PatchInfo> {
        self.patch_manager.next_boot_patch()
    }

    /// Performs integrity checks on the next boot patch. If the patch fails these checks, the patch
    /// will be deleted and the next boot patch will be set to the last successfully booted patch or
    /// the base release if there is no last successfully booted patch.
    ///
    /// Returns an error if the patch fails integrity checks.
    pub fn validate_next_boot_patch(&mut self) -> anyhow::Result<()> {
        self.patch_manager.validate_next_boot_patch()
    }

    /// Copies the patch file at file_path to the manager's directory structure sets
    /// this patch as the next patch to boot.
    pub fn install_patch(
        &mut self,
        patch: &PatchInfo,
        hash: &str,
        signature: Option<&str>,
    ) -> anyhow::Result<()> {
        self.patch_manager
            .add_patch(patch.number, &patch.path, hash, signature)
    }

    /// Removes the artifacts for patch `patch_number` from disk and updates state to ensure the
    /// uninstalled patch is not booted in the future.
    pub fn uninstall_patch(&mut self, patch_number: usize) -> Result<()> {
        self.patch_manager.remove_patch(patch_number)
    }

    /// Returns true if we have previously failed to boot from patch `patch_number`.
    pub fn is_known_bad_patch(&self, patch_number: usize) -> bool {
        self.patch_manager.is_known_bad_patch(patch_number)
    }
}

/// PatchEvent management
impl UpdaterState {
    /// Adds an event to the queue to be sent to the server.
    pub fn queue_event(&mut self, event: PatchEvent) -> Result<()> {
        self.serialized_state.queued_events.push(event);
        self.save()
    }

    /// Returns up to `limit` events from the reporting queue.
    pub fn copy_events(&self, limit: usize) -> Vec<PatchEvent> {
        self.serialized_state
            .queued_events
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Removes all events from the reporting queue.
    pub fn clear_events(&mut self) -> Result<()> {
        self.serialized_state.queued_events.clear();
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use tempdir::TempDir;

    use crate::cache::patch_manager::MockManagePatches;

    use mockall::predicate::eq;

    use super::*;

    fn test_state<MP>(tmp_dir: &TempDir, patch_manager: MP) -> UpdaterState
    where
        MP: ManagePatches + 'static,
    {
        UpdaterState {
            cache_dir: tmp_dir.path().to_path_buf(),
            patch_manager: Box::new(patch_manager),
            serialized_state: SerializedState {
                client_id: "123".to_string(),
                release_version: "1.0.0+1".to_string(),
                queued_events: Vec::new(),
            },
        }
    }

    fn fake_patch(tmp_dir: &TempDir, number: usize) -> super::PatchInfo {
        let path = tmp_dir.path().join(format!("patch_{}", number));
        std::fs::write(&path, "fake patch").unwrap();
        PatchInfo { number, path }
    }

    #[test]
    fn release_version_changed_resets_patches() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut patch_manager = PatchManager::manager_for_test(&tmp_dir);
        let file_path = &tmp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, "patch file contents").unwrap();
        assert!(patch_manager.add_patch(1, file_path, "hash", None).is_ok());

        let state = test_state(&tmp_dir, patch_manager);
        let release_version = state.serialized_state.release_version.clone();
        assert!(state.save().is_ok());

        let mut state =
            UpdaterState::load_or_new_on_error(&state.cache_dir, &release_version, None, PatchVerificationMode::default());
        assert_eq!(state.next_boot_patch().unwrap().number, 1);

        let mut next_version_state =
            UpdaterState::load_or_new_on_error(&state.cache_dir, "1.0.0+2", None, PatchVerificationMode::default());
        assert!(next_version_state.next_boot_patch().is_none());
    }

    #[test]
    fn is_file_not_found_test() {
        use anyhow::Context;
        assert!(!super::is_file_not_found(&anyhow::anyhow!("")));
        let tmp_dir = TempDir::new("example").unwrap();
        let path = tmp_dir.path().join("does_not_exist");
        let result = std::fs::File::open(path).context("foo");
        assert!(result.is_err());
        assert!(super::is_file_not_found(&result.unwrap_err()));
    }

    #[test]
    fn creates_updater_state_with_client_id() {
        let tmp_dir = TempDir::new("example").unwrap();
        let state = UpdaterState::load_or_new_on_error(tmp_dir.path(), "1.0.0+1", None, PatchVerificationMode::default());
        let saved_state = UpdaterState::load_or_new_on_error(tmp_dir.path(), "1.0.0+1", None, PatchVerificationMode::default());
        assert_eq!(
            state.serialized_state.client_id,
            saved_state.serialized_state.client_id
        );
    }

    // A new UpdaterState is created when the release version is changed, but
    // the client_id should remain the same.
    #[test]
    fn client_id_does_not_change_if_release_version_changes() {
        let tmp_dir = TempDir::new("example").unwrap();

        let state = test_state(&tmp_dir, PatchManager::manager_for_test(&tmp_dir));
        let original_loaded = UpdaterState::load_or_new_on_error(
            &state.cache_dir,
            &state.serialized_state.release_version,
            None,
            PatchVerificationMode::default(),
        );

        let new_loaded = UpdaterState::load_or_new_on_error(&state.cache_dir, "1.0.0+2", None, PatchVerificationMode::default());

        assert_eq!(
            original_loaded.serialized_state.client_id,
            new_loaded.serialized_state.client_id
        );
    }

    #[test]
    fn does_not_save_cache_dir() {
        let original_tmp_dir = TempDir::new("example").unwrap();
        let original_state = UpdaterState {
            cache_dir: original_tmp_dir.path().to_path_buf(),
            patch_manager: Box::new(PatchManager::manager_for_test(&original_tmp_dir)),
            serialized_state: SerializedState {
                client_id: "123".to_string(),
                release_version: "1.0.0+1".to_string(),
                queued_events: Vec::new(),
            },
        };
        original_state.save().unwrap();

        let new_tmp_dir = TempDir::new("example_2").unwrap();
        let original_state_path = original_tmp_dir.path().join(STATE_FILE_NAME);
        let new_state_path = new_tmp_dir.path().join(STATE_FILE_NAME);
        std::fs::rename(original_state_path, new_state_path).unwrap();

        let new_state = UpdaterState::load(new_tmp_dir.path(), None, PatchVerificationMode::default()).unwrap();
        assert_eq!(new_state.cache_dir, new_tmp_dir.path());
    }

    #[test]
    fn record_boot_failure_for_patch_forwards_to_patch_manager() {
        let patch_number = 1;
        let tmp_dir = TempDir::new("example").unwrap();
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_record_boot_failure_for_patch()
            .with(eq(patch_number))
            .returning(|_| Ok(()));
        let mut state = test_state(&tmp_dir, mock_manage_patches);
        assert!(state.record_boot_failure_for_patch(patch_number).is_ok());
    }

    #[test]
    fn record_boot_success_for_patch_forwards_to_patch_manager() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_record_boot_success()
            .returning(|| Ok(()));
        let mut state = test_state(&tmp_dir, mock_manage_patches);

        assert!(state.record_boot_success().is_ok());
    }

    #[test]
    fn last_successfully_booted_patch_forwards_from_patch_manager() {
        let tmp_dir = TempDir::new("example").unwrap();
        let patch = fake_patch(&tmp_dir, 1);
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_last_successfully_booted_patch()
            .return_const(Some(patch.clone()));
        let state = test_state(&tmp_dir, mock_manage_patches);
        assert_eq!(state.last_successfully_booted_patch(), Some(patch));
    }

    #[test]
    fn current_boot_patch_returns_currently_booting_patch_if_present() {
        let tmp_dir = TempDir::new("example").unwrap();
        let patch1 = fake_patch(&tmp_dir, 1);
        let patch2 = fake_patch(&tmp_dir, 2);
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_last_successfully_booted_patch()
            .return_const(Some(patch1.clone()));
        mock_manage_patches
            .expect_currently_booting_patch()
            .return_const(Some(patch2.clone()));
        let state = test_state(&tmp_dir, mock_manage_patches);
        assert_eq!(state.current_boot_patch(), Some(patch2));
    }

    #[test]
    fn current_boot_patch_returns_last_successfully_booted_patch_if_no_patch_is_booting() {
        let tmp_dir = TempDir::new("example").unwrap();
        let patch = fake_patch(&tmp_dir, 1);
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_last_successfully_booted_patch()
            .return_const(Some(patch.clone()));
        mock_manage_patches
            .expect_currently_booting_patch()
            .return_const(None);
        let state = test_state(&tmp_dir, mock_manage_patches);
        assert_eq!(state.current_boot_patch(), Some(patch));
    }

    #[test]
    fn next_boot_patch_forwards_from_patch_manager() {
        let patch_number = 1;
        let tmp_dir = TempDir::new("example").unwrap();
        let patch = fake_patch(&tmp_dir, patch_number);
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_next_boot_patch()
            .return_const(Some(patch.clone()));
        let mut state = test_state(&tmp_dir, mock_manage_patches);
        assert_eq!(state.next_boot_patch(), Some(patch));
    }

    #[test]
    fn validate_next_boot_patch_forwards_to_patch_manager() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_validate_next_boot_patch()
            .returning(|| Ok(()));
        let mut state = test_state(&tmp_dir, mock_manage_patches);
        assert!(state.validate_next_boot_patch().is_ok());
    }

    #[test]
    fn install_patch_forwards_to_patch_manager() {
        let patch_number = 1;
        let tmp_dir = TempDir::new("example").unwrap();
        let patch = fake_patch(&tmp_dir, patch_number);
        let mut mock_manage_patches = MockManagePatches::new();
        let cloned_patch = patch.clone();
        mock_manage_patches
            .expect_add_patch()
            .withf(move |number, path, hash, signature| {
                number == &cloned_patch.number
                    && path == cloned_patch.path
                    && hash == "hash"
                    && signature == &Some("signature")
            })
            .returning(|_, __, ___, ____| Ok(()));
        let mut state = test_state(&tmp_dir, mock_manage_patches);

        assert!(state
            .install_patch(&patch, "hash", Some("signature"))
            .is_ok());
    }

    #[test]
    fn is_known_bad_patch_returns_value_from_patch_manager() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_is_known_bad_patch()
            .with(eq(1))
            .return_const(true);
        mock_manage_patches
            .expect_is_known_bad_patch()
            .with(eq(2))
            .return_const(false);
        let state = test_state(&tmp_dir, mock_manage_patches);
        assert!(state.is_known_bad_patch(1));
        assert!(!state.is_known_bad_patch(2));
    }

    #[test]
    fn load_or_new_on_error_clears_patch_state_on_error() -> Result<()> {
        let tmp_dir = TempDir::new("example")?;

        // Create a new state, add a patch, and save it.
        let mut state = UpdaterState::load_or_new_on_error(&tmp_dir.path(), "1.0.0+1", None, PatchVerificationMode::default());
        let patch = fake_patch(&tmp_dir, 1);
        state.install_patch(&patch, "hash", None)?;
        state.save()?;
        assert_eq!(state.next_boot_patch().unwrap().number, 1);

        // Corrupt the state file.
        let state_file = tmp_dir.path().join(STATE_FILE_NAME);
        std::fs::write(&state_file, "corrupt json")?;

        // Ensure that, by corrupting the file, we've reset the patches state.
        let mut state = UpdaterState::load_or_new_on_error(&tmp_dir.path(), "1.0.0+2", None, PatchVerificationMode::default());
        assert!(state.next_boot_patch().is_none());

        Ok(())
    }
}
