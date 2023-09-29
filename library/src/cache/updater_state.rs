// This file deals with the cache / state management for the updater.

// This code is very confused and uses "patch number" sometimes
// and "slot index" others.  The public interface should be
// consistent and use patch number everywhere.
// PatchInfo can probably go away.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::events::PatchEvent;
use crate::updater::UpdateError;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as warn, println as debug}; // Workaround to use println! for logs.

use super::patch_manager::{ManagePatches, PatchManager};
use super::{disk_manager, PatchInfo};

/// Where the updater state is stored on disk.
const STATE_FILE_NAME: &str = "state.json";

// /// The private interface onto slots/patches within the cache.
// #[derive(Deserialize, Serialize, Default, Clone, Debug)]
// struct Slot {
//     /// Patch number for the patch in this slot.
//     patch_number: usize,
// }

/// Records the updater's "state of the world" - which patches we know to be
/// good or bad, which patches we have downloaded, which patch we're currently
/// booted from, events that need to be reported to the server, etc.
///
/// Written out to disk as a json file at STATE_FILE_NAME.
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

#[derive(Debug, Deserialize, Serialize)]
struct SerializedState {
    /// The client ID for this device.
    pub client_id: Option<String>,

    // Per-release state:
    /// The release version this cache corresponds to.
    /// If this does not match the release version we're booting from we will
    /// clear the cache.
    release_version: String,
    // /// List of patches that failed to boot.  We will never attempt these again.
    // failed_patches: Vec<usize>,
    // /// List of patches that successfully booted. We will never rollback past
    // /// one of these for this device.
    // successful_patches: Vec<usize>,
    // /// Slot that the app is currently booted from.
    // current_boot_slot_index: Option<usize>,
    // /// Slot that will be used for next boot.
    // next_boot_slot_index: Option<usize>,
    // /// List of slots.
    // slots: Vec<Slot>,
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

    pub fn client_id_or_default(&self) -> String {
        self.serialized_state
            .client_id
            .clone()
            .unwrap_or(String::new())
    }

    // pub fn is_known_good_patch(&self, patch_number: usize) -> bool {
    //     self.patch_manager.is_known_good_patch(patch_number)
    // }

    // pub fn is_known_bad_patch(&self, patch_number: usize) -> bool {
    //     self.patch_manager.is_known_bad_patch(patch_number)
    // }

    pub fn queue_event(&mut self, event: PatchEvent) {
        self.serialized_state.queued_events.push(event);
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

    pub fn mark_patch_as_bad(&mut self, patch_number: usize) -> Result<()> {
        self.patch_manager
            .record_boot_failure_for_patch(patch_number)
        // if self.is_known_good_patch(patch_number) {
        //     bail!("Tried to report failed launch for a known good patch.  Ignoring.");
        // }

        // if !self.is_known_bad_patch(patch_number) {
        //     // This is at least info! since we're in a failure state and want to log.
        //     info!("Marking patch {} as bad", patch_number);
        //     self.failed_patches.push(patch_number);
        // }
        // Ok(())
    }

    pub fn mark_patch_as_good(&mut self, patch_number: usize) -> Result<()> {
        self.patch_manager
            .record_boot_success_for_patch(patch_number)
        // if self.is_known_bad_patch(patch_number) {
        //     bail!("Tried to report successful launch for a known bad patch.  Ignoring.");
        // }

        // if !self.is_known_good_patch(patch_number) {
        //     self.successful_patches.push(patch_number);
        // }
        // Ok(())
    }

    fn load(cache_dir: &Path) -> anyhow::Result<Self> {
        // Load UpdaterState from disk
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
        let _ = state.save();
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
                // let validate_result = loaded.validate();
                // if let Err(e) = validate_result {
                //     warn!("Error while validating state: {:#}, clearing state.", e);
                //     return Self::create_new_and_save(
                //         storage_dir,
                //         release_version,
                //         maybe_client_id,
                //     );
                // }
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

    // fn patch_info_at(&self, index: usize) -> Option<PatchInfo> {
    //     if index >= self.slots.len() {
    //         return None;
    //     }
    //     let slot = &self.slots[index];
    //     // to_str only ever fails if the path is invalid utf8, which should
    //     // never happen, but this way we don't crash if it is.
    //     Some(PatchInfo {
    //         path: self.patch_path_for_index(index),
    //         number: slot.patch_number,
    //     })
    // }

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

    // fn validate(&mut self) -> anyhow::Result<()> {
    //     // iterate through all slots:
    //     // Make sure they're still valid.
    //     // If not, remove them.
    //     let slot_count = self.slots.len();
    //     let mut needs_save = false;
    //     // Iterate backwards so we can remove slots.
    //     for i in (0..slot_count).rev() {
    //         let slot = &self.slots[i];
    //         if !self.validate_slot(slot) {
    //             warn!("Slot {} is invalid, clearing.", i);
    //             self.clear_slot(i)?;
    //             needs_save = true;
    //         }
    //     }
    //     if needs_save {
    //         self.save()?;
    //     }
    //     Ok(())
    // }

    // fn validate_slot(&self, slot: &Slot) -> bool {
    //     // Check if the patch is known bad.
    //     if self.is_known_bad_patch(slot.patch_number) {
    //         debug!("Slot {:?} is known bad.", slot);
    //         return false;
    //     }
    //     let index = self
    //         .slots
    //         .iter()
    //         .position(|s| s.patch_number == slot.patch_number);
    //     let patch_path = self.patch_path_for_index(index.unwrap());
    //     if !patch_path.exists() {
    //         debug!("Slot {:?} {} does not exist.", slot, patch_path.display());
    //         return false;
    //     }
    //     // TODO: This should also check if the hash matches?
    //     // let hash = compute_hash(&PathBuf::from(&slot.path));
    //     // if let Ok(hash) = hash {
    //     //     if hash == slot.hash {
    //     //         return true;
    //     //     }
    //     //     error!("Hash mismatch for slot: {:?}", slot);
    //     // }
    //     true
    // }

    // fn latest_bootable_slot(&self) -> Option<usize> {
    //     // Find the latest slot that has a patch that is not bad.
    //     // Sort the slots by patch number, then return the highest
    //     // patch number that is not bad.
    //     let mut slots = self.slots.clone();
    //     slots.sort_by(|a, b| a.patch_number.cmp(&b.patch_number));
    //     slots.reverse();
    //     for slot in slots {
    //         if self.validate_slot(&slot) {
    //             return Some(slot.patch_number);
    //         }
    //     }
    //     None
    // }

    /// Sets the patch we will boot from on the next run to the latest non-bad
    /// patch we know about.
    // pub fn activate_latest_bootable_patch(&mut self) -> Result<(), UpdateError> {
    //     self.patch_manager
    //         .set_next_patch_to_latest_bootable()
    //         // TODO this map_err should not be here, it should be in the patch manager.
    //         .map_err(|_| UpdateError::FailedToSaveState)
    //     // self.set_next_boot_patch_slot(self.latest_bootable_slot());
    //     // self.save().map_err(|_| UpdateError::FailedToSaveState)
    // }

    // fn available_slot(&self) -> usize {
    //     // Assume we only use two slots and pick the one that's not current.
    //     if self.slots.is_empty() {
    //         return 0;
    //     }
    //     if let Some(slot_index) = self.current_boot_slot_index {
    //         // This does not check next_boot_slot_index, we're assuming that
    //         // whoever is calling this is OK with replacing the next boot
    //         // patch.
    //         if slot_index == 0 {
    //             return 1;
    //         }
    //     }
    //     0
    // }

    // fn clear_slot(&mut self, index: usize) -> anyhow::Result<()> {
    //     // Index is outside of the slots we have.
    //     if index >= self.slots.len() {
    //         // Ignore slots past the end for now?
    //         return Ok(());
    //     }
    //     self.slots[index] = Slot::default();
    //     let slot_dir_string = self.slot_dir_for_index(index);
    //     if slot_dir_string.exists() {
    //         std::fs::remove_dir_all(&slot_dir_string)?;
    //     }
    //     Ok(())
    // }

    // fn set_slot(&mut self, index: usize, slot: Slot) {
    //     debug!("Setting slot {} to {:?}", index, slot);
    //     if self.slots.len() < index + 1 {
    //         // Make sure we're not filling with empty slots.
    //         assert!(self.slots.len() == index);
    //         self.slots.resize(index + 1, Slot::default());
    //     }
    //     // Set the given slot to the given version.
    //     self.slots[index] = slot;
    // }

    // fn patch_path_for_index(&self, index: usize) -> PathBuf {
    //     self.slot_dir_for_index(index).join("dlc.vmcode")
    // }

    // fn slot_dir_for_index(&self, index: usize) -> PathBuf {
    //     Path::new(&self.cache_dir).join(format!("slot_{index}"))
    // }

    pub fn install_patch(&mut self, patch: &PatchInfo) -> anyhow::Result<()> {
        self.patch_manager.add_patch(patch.number, &patch.path)
        // let slot_index = self.available_slot();
        // let slot_dir_string = self.slot_dir_for_index(slot_index);
        // let slot_dir = PathBuf::from(&slot_dir_string);

        // // Clear the slot.
        // self.clear_slot(slot_index)?; // Invalidate the slot.
        // self.save()?;
        // std::fs::create_dir_all(&slot_dir)
        //     .with_context(|| format!("create_dir_all failed for {}", slot_dir.display()))?;

        // if self.is_known_bad_patch(patch.number) {
        //     return Err(UpdateError::InvalidArgument(
        //         "patch".to_owned(),
        //         format!("Refusing to install known bad patch: {patch:?}"),
        //     )
        //     .into());
        // }

        // // Move the artifact into the slot.
        // let artifact_path = slot_dir.join("dlc.vmcode");
        // std::fs::rename(&patch.path, artifact_path)?;

        // // Update the state to include the new slot.
        // self.set_slot(
        //     slot_index,
        //     Slot {
        //         patch_number: patch.number,
        //     },
        // );
        // self.set_next_boot_patch_slot(Some(slot_index));

        // if let Some(latest) = self.latest_patch_number() {
        //     if patch.number < latest {
        //         warn!(
        //             "Installed patch {} but latest downloaded patch is {latest:?}",
        //             patch.number
        //         );
        //     }
        // }
        // self.save()?;

        // let path = self.patch_path_for_index(slot_index);
        // if path.exists() {
        //     debug!("Patch {} installed to {:?}", patch.number, path);
        // } else {
        //     warn!(
        //         "Patch {} installed but does not exist {:?}",
        //         patch.number, path
        //     );
        // }

        // Ok(())
    }

    // Sets the `current_boot` slot to the `next_boot` slot.
    // pub fn activate_current_patch(&mut self) -> Result<(), UpdateError> {
    //     self.patch_manager
    //         .set_current_patch_to_next()
    //         .map_err(|_| UpdateError::InvalidState("No patch to activate.".to_owned()))
    //     // if self.next_boot_slot_index.is_none() {
    //     //     return Err(UpdateError::InvalidState(
    //     //         "No patch to activate.".to_owned(),
    //     //     ));
    //     // }
    //     // self.current_boot_slot_index = self.next_boot_slot_index;
    //     // assert!(self.current_boot_slot_index.is_some());
    //     // Ok(())
    // }

    // /// Switches the next boot slot to the given slot or clears it if None.
    // fn set_next_boot_patch_slot(&mut self, maybe_index: Option<usize>) {
    //     self.next_boot_slot_index = maybe_index;
    // }

    /// Returns highest patch number that has been installed for this release.
    /// This should represent the latest patch we still have on disk so as
    /// to prevent re-downloading patches we already have.
    /// This should essentially be the max of the patch number in the slots
    /// and the bad patch list (we don't need to keep bad patches on disk
    /// to know that they're bad).
    /// Used by the patch check logic.
    pub fn latest_patch_number(&self) -> Option<usize> {
        self.patch_manager.highest_seen_patch_number()
        // Get the max of the patch numbers in the slots.
        // // We probably could do this with chain and max?
        // let installed_max = self.slots.iter().map(|s| s.patch_number).max();
        // let failed_max = self.failed_patches.clone().into_iter().max();
        // match installed_max {
        //     None => failed_max,
        //     Some(installed) => match failed_max {
        //         None => installed_max,
        //         Some(failed) => Some(std::cmp::max(installed, failed)),
        //     },
        // }
    }
}

// #[cfg(test)]
// mod tests {
//     use tempdir::TempDir;

//     use super::{PatchInfo, UpdaterState, STATE_FILE_NAME};

//     fn test_state(tmp_dir: &TempDir) -> UpdaterState {
//         let cache_dir = tmp_dir.path();
//         UpdaterState::new(cache_dir.to_owned(), "1.0.0+1".to_string(), None)
//     }

//     fn fake_patch(tmp_dir: &TempDir, number: usize) -> super::PatchInfo {
//         let path = tmp_dir.path().join(format!("patch_{}", number));
//         std::fs::write(&path, "fake patch").unwrap();
//         PatchInfo { number, path }
//     }

//     #[test]
//     fn next_boot_patch_does_not_crash() {
//         let tmp_dir = TempDir::new("example").unwrap();
//         let mut state = test_state(&tmp_dir);
//         assert_eq!(state.next_boot_patch(), None);
//         state.next_boot_slot_index = Some(3);
//         assert_eq!(state.next_boot_patch(), None);
//         state.slots.push(super::Slot::default());
//         // This used to crash, where index was bad, but slots were not empty.
//         assert_eq!(state.next_boot_patch(), None);
//     }

//     #[test]
//     fn release_version_changed() {
//         let tmp_dir = TempDir::new("example").unwrap();
//         let mut state = test_state(&tmp_dir);
//         state.next_boot_slot_index = Some(1);
//         state.save().unwrap();
//         let loaded = UpdaterState::load_or_new_on_error(&state.cache_dir, &state.release_version);
//         assert_eq!(loaded.next_boot_slot_index, Some(1));

//         let loaded_after_version_change =
//             UpdaterState::load_or_new_on_error(&state.cache_dir, "1.0.0+2");
//         assert_eq!(loaded_after_version_change.next_boot_slot_index, None);
//     }

//     #[test]
//     fn latest_downloaded_patch() {
//         let tmp_dir = TempDir::new("example").unwrap();
//         let mut state = test_state(&tmp_dir);
//         assert_eq!(state.latest_patch_number(), None);
//         state.install_patch(&fake_patch(&tmp_dir, 1)).unwrap();
//         assert_eq!(state.latest_patch_number(), Some(1));
//         state.install_patch(&fake_patch(&tmp_dir, 2)).unwrap();
//         assert_eq!(state.latest_patch_number(), Some(2));
//         state.install_patch(&fake_patch(&tmp_dir, 1)).unwrap();
//         // This probably should be Some(2) assuming we didn't write
//         // over the top of patch 2 when re-installing patch 1.
//         // I expect if we support rollbacks we might be more explicit
//         // that it's a rollback?
//         assert_eq!(state.latest_patch_number(), Some(1));
//     }

//     #[test]
//     fn do_not_install_known_bad_patch() {
//         let tmp_dir = TempDir::new("example").unwrap();
//         let mut state = test_state(&tmp_dir);
//         let bad_patch = fake_patch(&tmp_dir, 1);
//         state.mark_patch_as_bad(bad_patch.number).unwrap();
//         let number = bad_patch.number;
//         assert!(state.install_patch(&bad_patch).is_err());

//         // Calling a second time should not error.
//         state.mark_patch_as_bad(number).unwrap();
//     }

//     #[test]
//     fn do_not_mark_bad_patch_good() {
//         let tmp_dir = TempDir::new("example").unwrap();
//         let mut state = test_state(&tmp_dir);
//         let bad_patch = fake_patch(&tmp_dir, 1);
//         assert!(state.mark_patch_as_bad(bad_patch.number).is_ok());
//         assert!(state.mark_patch_as_good(bad_patch.number).is_err());
//         assert!(state.is_known_bad_patch(bad_patch.number));
//         assert!(!state.is_known_good_patch(bad_patch.number));
//     }

//     #[test]
//     fn mark_patch_as_good() {
//         let tmp_dir = TempDir::new("example").unwrap();
//         let mut state = test_state(&tmp_dir);
//         let patch = fake_patch(&tmp_dir, 1);
//         state.mark_patch_as_good(patch.number).unwrap();
//         assert!(state.is_known_good_patch(patch.number));
//         assert!(!state.is_known_bad_patch(patch.number));
//         // Marking it twice doesn't change anything.
//         state.mark_patch_as_good(patch.number).unwrap();
//         assert!(state.is_known_good_patch(patch.number));
//         assert!(!state.is_known_bad_patch(patch.number));
//     }

//     #[test]
//     fn is_file_not_found_test() {
//         use anyhow::Context;
//         assert!(!super::is_file_not_found(&anyhow::anyhow!("")));
//         let tmp_dir = TempDir::new("example").unwrap();
//         let path = tmp_dir.path().join("does_not_exist");
//         let result = std::fs::File::open(path).context("foo");
//         assert!(result.is_err());
//         assert!(super::is_file_not_found(&result.unwrap_err()));
//     }

//     #[test]
//     fn creates_updater_state_with_client_id() {
//         let tmp_dir = TempDir::new("example").unwrap();
//         let state = UpdaterState::load_or_new_on_error(tmp_dir.path(), "1.0.0+1");
//         assert!(state.client_id.is_some());
//         let saved_state = UpdaterState::load_or_new_on_error(tmp_dir.path(), "1.0.0+1");
//         assert_eq!(state.client_id, saved_state.client_id);
//     }

//     #[test]
//     fn adds_client_id_to_saved_state() {
//         let tmp_dir = TempDir::new("example").unwrap();
//         let state = UpdaterState {
//             cache_dir: tmp_dir.path().to_path_buf(),
//             release_version: "1.0.0+1".to_string(),
//             client_id: None,
//             queued_events: Vec::new(),
//             current_boot_slot_index: None,
//             next_boot_slot_index: None,
//             failed_patches: Vec::new(),
//             successful_patches: Vec::new(),
//             slots: Vec::new(),
//         };

//         state.save().unwrap();

//         let loaded_state =
//             UpdaterState::load_or_new_on_error(&state.cache_dir, &state.release_version);
//         assert!(loaded_state.client_id.is_some());
//     }

//     // A new UpdaterState is created when the release version is changed, but
//     // the client_id should remain the same.
//     #[test]
//     fn client_id_does_not_change_if_release_version_changes() {
//         let tmp_dir = TempDir::new("example").unwrap();

//         let original_state = UpdaterState {
//             cache_dir: tmp_dir.path().to_path_buf(),
//             release_version: "1.0.0+1".to_string(),
//             client_id: None,
//             queued_events: Vec::new(),
//             current_boot_slot_index: None,
//             next_boot_slot_index: None,
//             failed_patches: Vec::new(),
//             successful_patches: Vec::new(),
//             slots: Vec::new(),
//         };
//         let original_loaded = UpdaterState::load_or_new_on_error(
//             &original_state.cache_dir,
//             &original_state.release_version,
//         );

//         let new_loaded = UpdaterState::load_or_new_on_error(&original_state.cache_dir, "1.0.0+2");

//         assert!(original_loaded.client_id.is_some());
//         assert!(new_loaded.client_id.is_some());
//         assert_eq!(original_loaded.client_id, new_loaded.client_id);
//     }

//     #[test]
//     fn does_not_save_cache_dir() {
//         let original_tmp_dir = TempDir::new("example").unwrap();
//         let original_state = UpdaterState {
//             cache_dir: original_tmp_dir.path().to_path_buf(),
//             release_version: "1.0.0+1".to_string(),
//             client_id: None,
//             queued_events: Vec::new(),
//             current_boot_slot_index: None,
//             next_boot_slot_index: None,
//             failed_patches: Vec::new(),
//             successful_patches: Vec::new(),
//             slots: Vec::new(),
//         };
//         original_state.save().unwrap();

//         let new_tmp_dir = TempDir::new("example_2").unwrap();
//         let original_state_path = original_tmp_dir.path().join(STATE_FILE_NAME);
//         let new_state_path = new_tmp_dir.path().join(STATE_FILE_NAME);
//         std::fs::rename(original_state_path, new_state_path).unwrap();

//         let new_state = UpdaterState::load(new_tmp_dir.path()).unwrap();
//         assert_eq!(new_state.cache_dir, new_tmp_dir.path());
//         assert_eq!(
//             new_state.slot_dir_for_index(1),
//             new_tmp_dir.path().join("slot_1").to_path_buf()
//         );
//     }
// }
