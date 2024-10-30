// This file deals with the cache / state management for the updater.

// This code is very confused and uses "patch number" sometimes
// and "slot index" others.  The public interface should be
// consistent and use patch number everywhere.
// PatchInfo can probably go away.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::config::UpdateConfig;
use crate::events::PatchEvent;

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
    // Per-release state:
    /// The release version this cache corresponds to.
    /// If this does not match the release version we're booting from we will
    /// clear the cache.
    release_version: String,
    /// Events that have not yet been sent to the server.
    /// Format could change between releases, so this is per-release state.
    queued_events: Vec<PatchEvent>,
    /// A randomly assigned number between 1 and 100 (inclusive) that determines when this device
    /// will receive a phased rollout. If the rollout_group is less than or equal to the rollout
    /// percentage, the device will receive the update (this logic is implemented server-side).
    ///
    /// This number is generated once when the state is created (i.e., when a release is first
    /// launched) and is not changed until the next release is installed.
    rollout_group: u32,
}

fn is_file_not_found(error: &anyhow::Error) -> bool {
    for cause in error.chain() {
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            return io_error.kind() == std::io::ErrorKind::NotFound;
        }
    }
    false
}

/// Lifecycle methods for the updater state.
impl UpdaterState {
    /// Creates a new `UpdaterState`.
    fn new(cache_dir: PathBuf, release_version: String, patch_public_key: Option<&str>) -> Self {
        Self {
            cache_dir: cache_dir.clone(),
            patch_manager: Box::new(PatchManager::new(cache_dir.clone(), patch_public_key)),
            serialized_state: SerializedState {
                release_version,
                queued_events: Vec::new(),
                // Generate random number in the range [1, 100].
                rollout_group: rand::thread_rng().gen_range(1..101),
            },
        }
    }

    /// Loads UpdaterState from disk
    fn load(cache_dir: &Path, patch_public_key: Option<&str>) -> anyhow::Result<Self> {
        let path = cache_dir.join(STATE_FILE_NAME);
        let serialized_state = disk_io::read(&path)?;
        Ok(UpdaterState {
            cache_dir: cache_dir.to_path_buf(),
            patch_manager: Box::new(PatchManager::new(cache_dir.to_path_buf(), patch_public_key)),
            serialized_state,
        })
    }

    /// Initializes a new UpdaterState and saves it to disk.
    fn create_new_and_save(
        storage_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
    ) -> Self {
        let state = Self::new(
            storage_dir.to_owned(),
            release_version.to_owned(),
            patch_public_key,
        );
        if let Err(e) = state.save() {
            shorebird_warn!("Error saving state {:?}, ignoring.", e);
        }
        state
    }

    pub fn load_or_new_from_config(config: &UpdateConfig) -> Self {
        UpdaterState::load_or_new_on_error(
            &config.storage_dir,
            &config.release_version,
            config.patch_public_key.as_deref(),
        )
    }

    pub fn load_or_new_on_error(
        storage_dir: &Path,
        release_version: &str,
        patch_public_key: Option<&str>,
    ) -> Self {
        let load_result = Self::load(storage_dir, patch_public_key);
        match load_result {
            Ok(mut loaded) => {
                if loaded.serialized_state.release_version != release_version {
                    shorebird_info!(
                        "release_version changed {} -> {}, creating new state",
                        loaded.serialized_state.release_version,
                        release_version
                    );
                    let _ = loaded.patch_manager.reset();
                    return Self::create_new_and_save(
                        storage_dir,
                        release_version,
                        patch_public_key,
                    );
                }
                loaded
            }
            Err(e) => {
                if !is_file_not_found(&e) {
                    shorebird_info!("No existing state file found: {:#}, creating new state.", e);
                }
                Self::create_new_and_save(storage_dir, release_version, patch_public_key)
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

    /// The rollout group number (1-100) for this device.
    pub fn rollout_group(&self) -> u32 {
        self.serialized_state.rollout_group
    }

    /// This is the patch that will be used for the next boot.
    /// Will be None if:
    /// - There has never been a patch selected.
    /// - There was a patch selected but it was later marked as bad.
    pub fn next_boot_patch(&mut self) -> Option<PatchInfo> {
        self.patch_manager.next_boot_patch()
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
                release_version: "1.0.0+1".to_string(),
                queued_events: Vec::new(),
                rollout_group: 1,
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
            UpdaterState::load_or_new_on_error(&state.cache_dir, &release_version, None);
        assert_eq!(state.next_boot_patch().unwrap().number, 1);

        let mut next_version_state =
            UpdaterState::load_or_new_on_error(&state.cache_dir, "1.0.0+2", None);
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
    fn does_not_save_cache_dir() {
        let original_tmp_dir = TempDir::new("example").unwrap();
        let original_state = UpdaterState {
            cache_dir: original_tmp_dir.path().to_path_buf(),
            patch_manager: Box::new(PatchManager::manager_for_test(&original_tmp_dir)),
            serialized_state: SerializedState {
                release_version: "1.0.0+1".to_string(),
                queued_events: Vec::new(),
                rollout_group: 10,
            },
        };
        original_state.save().unwrap();

        let new_tmp_dir = TempDir::new("example_2").unwrap();
        let original_state_path = original_tmp_dir.path().join(STATE_FILE_NAME);
        let new_state_path = new_tmp_dir.path().join(STATE_FILE_NAME);
        std::fs::rename(original_state_path, new_state_path).unwrap();

        let new_state = UpdaterState::load(new_tmp_dir.path(), None).unwrap();
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
    fn generates_random_rollout_group_between_1_and_100() {
        let tmp_dir = TempDir::new("example").unwrap();
        let state = test_state(&tmp_dir, PatchManager::manager_for_test(&tmp_dir));
        let first_rollout_group = state.serialized_state.rollout_group;
        assert!(first_rollout_group >= 1);
        assert!(first_rollout_group <= 100);

        let number_of_tries = 5;
        for i in 0..number_of_tries {
            let state = test_state(&tmp_dir, PatchManager::manager_for_test(&tmp_dir));
            assert!(state.serialized_state.rollout_group >= 1);
            assert!(state.serialized_state.rollout_group <= 100);
            if state.serialized_state.rollout_group == first_rollout_group {
                // This is an unlikely event, but it could happen.
                // If it does, we'll try a few more times.
                continue;
            }

            if i == number_of_tries - 1 {
                // The likelihood of getting the same random 1-100 number 5 times in a row is 1 in
                // 100^5, or 10,000,000,000.
                // Treat this as a failure of our random number generation.
                assert!(
                    false,
                    "Failed to generate a random rollout group after 5 tries."
                );
            }
        }
    }
}
