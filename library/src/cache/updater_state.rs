// This file deals with the cache / state management for the updater.

// This code is very confused and uses "patch number" sometimes
// and "slot index" others.  The public interface should be
// consistent and use patch number everywhere.
// PatchInfo can probably go away.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::events::PatchEvent;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as warn}; // Workaround to use println! for logs.

use super::patch_manager::{ManagePatches, PatchManager};
use super::{disk_manager, PatchInfo};

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
    /// The client ID for this device.
    pub client_id: Option<String>,

    // Per-release state:
    /// The release version this cache corresponds to.
    /// If this does not match the release version we're booting from we will
    /// clear the cache.
    release_version: String,
    /// Events that have not yet been sent to the server.
    /// Format could change between releases, so this is per-release state.
    queued_events: Vec<PatchEvent>,
}

fn is_file_not_found(error: &anyhow::Error) -> bool {
    for cause in error.chain() {
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            return io_error.kind() == std::io::ErrorKind::NotFound;
        }
    }
    false
}

fn generate_client_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Lifecycle methods for the updater state.
impl UpdaterState {
    /// Creates a new `UpdaterState`. If `client_id` is None, a new one will be generated.
    fn new(cache_dir: PathBuf, release_version: String, client_id: Option<String>) -> Self {
        Self {
            cache_dir: cache_dir.clone(),
            patch_manager: Box::new(PatchManager::with_root_dir(cache_dir.clone())),
            serialized_state: SerializedState {
                client_id: client_id.or(Some(generate_client_id())),
                release_version,
                queued_events: Vec::new(),
            },
        }
    }

    /// Loads UpdaterState from disk
    fn load(cache_dir: &Path) -> anyhow::Result<Self> {
        let path = cache_dir.join(STATE_FILE_NAME);
        let serialized_state = disk_manager::read(&path)?;
        let mut state = UpdaterState {
            cache_dir: cache_dir.to_path_buf(),
            patch_manager: Box::new(PatchManager::with_root_dir(cache_dir.to_path_buf())),
            serialized_state,
        };
        if state.serialized_state.client_id.is_none() {
            // Generate a client id if we don't already have one.
            state.serialized_state.client_id = Some(generate_client_id());
            let _ = state.save();
        }
        Ok(state)
    }

    /// Initializes a new UpdaterState and saves it to disk.
    fn create_new_and_save(
        storage_dir: &Path,
        release_version: &str,
        client_id: Option<String>,
    ) -> Self {
        let state = Self::new(
            storage_dir.to_owned(),
            release_version.to_owned(),
            client_id,
        );
        if let Err(e) = state.save() {
            warn!("Error saving state {:?}, ignoring.", e);
        }
        state
    }

    pub fn load_or_new_on_error(storage_dir: &Path, release_version: &str) -> Self {
        let load_result = Self::load(storage_dir);
        match load_result {
            Ok(mut loaded) => {
                let maybe_client_id = loaded.serialized_state.client_id.clone();
                if loaded.serialized_state.release_version != release_version {
                    info!(
                        "release_version changed {} -> {}, clearing updater state",
                        loaded.serialized_state.release_version, release_version
                    );
                    let _ = loaded.patch_manager.reset();
                    return Self::create_new_and_save(
                        storage_dir,
                        release_version,
                        maybe_client_id,
                    );
                }
                loaded
            }
            Err(e) => {
                if !is_file_not_found(&e) {
                    warn!("Error loading state: {:#}, clearing state.", e);
                }
                Self::create_new_and_save(storage_dir, release_version, None)
            }
        }
    }

    /// Saves the updater state to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Path::new(&self.cache_dir).join(STATE_FILE_NAME);
        disk_manager::write(&self.serialized_state, &path)
    }
}

/// Serialized updater state
impl UpdaterState {
    pub fn client_id_or_default(&self) -> String {
        self.serialized_state
            .client_id
            .clone()
            .unwrap_or(String::new())
    }
}

/// Patch management. All patch management is done via the patch manager.
impl UpdaterState {
    /// Records that the patch with patch_number failed to boot, uninstalls the patch.
    pub fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()> {
        self.patch_manager
            .record_boot_failure_for_patch(patch_number)
    }

    /// Records that the patch with patch_number was successfully booted, marks the patch as "good".
    pub fn record_boot_success_for_patch(&mut self, patch_number: usize) -> Result<()> {
        self.patch_manager
            .record_boot_success_for_patch(patch_number)
    }

    /// This is the current patch that is running.
    /// Will be None if:
    /// - There was no good patch at time of boot.
    /// - The updater has been initialized but no boot recorded yet.
    pub fn current_boot_patch(&self) -> Option<PatchInfo> {
        self.patch_manager.last_successfully_booted_patch()
    }

    /// This is the patch that will be used for the next boot.
    /// Will be None if:
    /// - There has never been a patch selected.
    /// - There was a patch selected but it was later marked as bad.
    pub fn next_boot_patch(&mut self) -> Option<PatchInfo> {
        self.patch_manager.get_next_boot_patch()
    }

    /// Copies the patch file at file_path to the manager's directory structure sets
    /// this patch as the next patch to boot.
    pub fn install_patch(&mut self, patch: &PatchInfo) -> anyhow::Result<()> {
        self.patch_manager.add_patch(patch.number, &patch.path)
    }

    /// Returns highest patch number that has been installed for this release.
    /// This should represent the latest patch we still have on disk so as
    /// to prevent re-downloading patches we already have.
    /// This should essentially be the max of the patch number in the slots
    /// and the bad patch list (we don't need to keep bad patches on disk
    /// to know that they're bad).
    /// Used by the patch check logic.
    pub fn latest_seen_patch_number(&self) -> Option<usize> {
        self.patch_manager.highest_seen_patch_number()
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
                release_version: "1.0.0+1".to_string(),
                client_id: None,
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
        let mut patch_manager = PatchManager::with_root_dir(tmp_dir.path().to_path_buf());
        let file_path = &tmp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, "patch file contents").unwrap();
        assert!(patch_manager.add_patch(1, file_path).is_ok());

        let state = test_state(&tmp_dir, patch_manager);
        let release_version = state.serialized_state.release_version.clone();
        assert!(state.save().is_ok());

        let mut state = UpdaterState::load_or_new_on_error(&state.cache_dir, &release_version);
        assert_eq!(state.next_boot_patch().unwrap().number, 1);

        let mut next_version_state =
            UpdaterState::load_or_new_on_error(&state.cache_dir, "1.0.0+2");
        assert!(next_version_state.next_boot_patch().is_none());
        assert!(next_version_state.latest_seen_patch_number().is_none());
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
        let state = UpdaterState::load_or_new_on_error(tmp_dir.path(), "1.0.0+1");
        assert!(state.serialized_state.client_id.is_some());
        let saved_state = UpdaterState::load_or_new_on_error(tmp_dir.path(), "1.0.0+1");
        assert_eq!(
            state.serialized_state.client_id,
            saved_state.serialized_state.client_id
        );
    }

    #[test]
    fn adds_client_id_to_saved_state() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mock_manage_patches = MockManagePatches::new();
        let state = test_state(&tmp_dir, mock_manage_patches);

        assert!(state.save().is_ok());

        let loaded_state = UpdaterState::load_or_new_on_error(
            &state.cache_dir,
            &state.serialized_state.release_version,
        );
        assert!(loaded_state.serialized_state.client_id.is_some());
    }

    // A new UpdaterState is created when the release version is changed, but
    // the client_id should remain the same.
    #[test]
    fn client_id_does_not_change_if_release_version_changes() {
        let tmp_dir = TempDir::new("example").unwrap();

        let state = test_state(
            &tmp_dir,
            PatchManager::with_root_dir(tmp_dir.path().to_path_buf()),
        );
        let original_loaded = UpdaterState::load_or_new_on_error(
            &state.cache_dir,
            &state.serialized_state.release_version,
        );

        let new_loaded = UpdaterState::load_or_new_on_error(&state.cache_dir, "1.0.0+2");

        assert!(original_loaded.serialized_state.client_id.is_some());
        assert!(new_loaded.serialized_state.client_id.is_some());
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
            patch_manager: Box::new(PatchManager::with_root_dir(
                original_tmp_dir.path().to_path_buf(),
            )),
            serialized_state: SerializedState {
                release_version: "1.0.0+1".to_string(),
                client_id: None,
                queued_events: Vec::new(),
            },
        };
        original_state.save().unwrap();

        let new_tmp_dir = TempDir::new("example_2").unwrap();
        let original_state_path = original_tmp_dir.path().join(STATE_FILE_NAME);
        let new_state_path = new_tmp_dir.path().join(STATE_FILE_NAME);
        std::fs::rename(original_state_path, new_state_path).unwrap();

        let new_state = UpdaterState::load(new_tmp_dir.path()).unwrap();
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
        let patch_number = 1;
        let tmp_dir = TempDir::new("example").unwrap();
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_record_boot_success_for_patch()
            .with(eq(patch_number))
            .returning(|_| Ok(()));
        let mut state = test_state(&tmp_dir, mock_manage_patches);

        assert!(state.record_boot_success_for_patch(patch_number).is_ok());
    }

    #[test]
    fn current_boot_patch_forwards_from_patch_manager() {
        let tmp_dir = TempDir::new("example").unwrap();
        let patch = fake_patch(&tmp_dir, 1);
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_last_successfully_booted_patch()
            .return_const(Some(patch.clone()));
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
            .expect_get_next_boot_patch()
            .return_const(Some(patch.clone()));
        let mut state = test_state(&tmp_dir, mock_manage_patches);
        assert_eq!(state.next_boot_patch(), Some(patch));
    }

    #[test]
    fn install_patch_forwards_to_patch_manager() {
        let patch_number = 1;
        let tmp_dir = TempDir::new("example").unwrap();
        let patch = fake_patch(&tmp_dir, patch_number);
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_add_patch()
            .with(eq(patch.number), eq(patch.path.clone()))
            .returning(|_, __| Ok(()));
        let mut state = test_state(&tmp_dir, mock_manage_patches);

        assert!(state.install_patch(&patch).is_ok());
    }

    #[test]
    fn latest_patch_number_returns_value_from_patch_manager() {
        let highest_patch_number = 1;
        let tmp_dir = TempDir::new("example").unwrap();
        let mut mock_manage_patches = MockManagePatches::new();
        mock_manage_patches
            .expect_highest_seen_patch_number()
            .return_const(Some(highest_patch_number));
        let state = test_state(&tmp_dir, mock_manage_patches);
        assert_eq!(state.latest_seen_patch_number(), Some(highest_patch_number));
    }
}
