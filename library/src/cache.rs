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

use crate::updater::UpdateError;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as warn, println as debug}; // Workaround to use println! for logs.

/// The public interace for talking about patches to the Cache.
#[derive(PartialEq, Debug)]
pub struct PatchInfo {
    pub path: PathBuf,
    pub number: usize,
}

/// The private interface onto slots/patches within the cache.
#[derive(Deserialize, Serialize, Default, Clone, Debug)]
struct Slot {
    /// Patch number for the patch in this slot.
    patch_number: usize,
}

// This struct is public, as callers can have a handle to it, but modifying
// anything inside should be done via the functions below.
#[derive(Deserialize, Serialize)]
pub struct UpdaterState {
    /// Where this writes to disk.
    cache_dir: PathBuf,
    /// The release version this cache corresponds to.
    /// If this does not match the release version we're booting from we will
    /// clear the cache.
    release_version: String,
    /// List of patches that failed to boot.  We will never attempt these again.
    failed_patches: Vec<usize>,
    /// List of patches that successfully booted. We will never rollback past
    /// one of these for this device.
    successful_patches: Vec<usize>,
    /// Slot that the app is currently booted from.
    current_boot_slot_index: Option<usize>,
    /// Slot that will be used for next boot.
    next_boot_slot_index: Option<usize>,
    /// List of slots.
    slots: Vec<Slot>,
    /// The client ID for this device.
    pub client_id: Option<String>,
    // Add file path or FD so modifying functions can save it to disk?
}

impl UpdaterState {
    fn new(cache_dir: PathBuf, release_version: String) -> Self {
        Self {
            cache_dir,
            release_version,
            client_id: Some(generate_client_id()),
            current_boot_slot_index: None,
            next_boot_slot_index: None,
            failed_patches: Vec::new(),
            successful_patches: Vec::new(),
            slots: Vec::new(),
        }
    }
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
    pub fn is_known_good_patch(&self, patch_number: usize) -> bool {
        self.successful_patches.iter().any(|v| v == &patch_number)
    }

    pub fn is_known_bad_patch(&self, patch_number: usize) -> bool {
        self.failed_patches.iter().any(|v| v == &patch_number)
    }

    pub fn mark_patch_as_bad(&mut self, patch_number: usize) -> Result<()> {
        if self.is_known_good_patch(patch_number) {
            bail!("Tried to report failed launch for a known good patch.  Ignoring.");
        }

        if !self.is_known_bad_patch(patch_number) {
            // This is at least info! since we're in a failure state and want to log.
            info!("Marking patch {} as bad", patch_number);
            self.failed_patches.push(patch_number);
        }
        Ok(())
    }

    // TODO(eseidel): Should return Result instead of logging, the c_api
    // layer can log if desired.
    pub fn mark_patch_as_good(&mut self, patch_number: usize) -> Result<()> {
        if self.is_known_bad_patch(patch_number) {
            bail!("Tried to report successful launch for a known bad patch.  Ignoring.");
        }

        if !self.is_known_good_patch(patch_number) {
            self.successful_patches.push(patch_number);
        }
        Ok(())
    }

    fn load(cache_dir: &Path) -> anyhow::Result<Self> {
        // Load UpdaterState from disk
        let path = cache_dir.join("state.json");
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        // TODO: Now that we depend on serde_yaml for shorebird.yaml
        // we could use yaml here instead of json.
        let mut state: UpdaterState = serde_json::from_reader(reader)?;
        if state.client_id.is_none() {
            // Generate a client id if we don't already have one.
            state.client_id = Some(generate_client_id());
        }
        Ok(state)
    }

    pub fn load_or_new_on_error(cache_dir: &Path, release_version: &str) -> Self {
        let load_result = Self::load(cache_dir);
        match load_result {
            Ok(mut loaded) => {
                if loaded.release_version != release_version {
                    info!(
                        "release_version changed {} -> {}, clearing updater state",
                        loaded.release_version, release_version
                    );
                    return Self::new(cache_dir.to_owned(), release_version.to_owned());
                }
                let validate_result = loaded.validate();
                if let Err(e) = validate_result {
                    warn!("Error while validating state: {:#}, clearing state.", e);
                    return Self::new(cache_dir.to_owned(), release_version.to_owned());
                }
                loaded
            }
            Err(e) => {
                if !is_file_not_found(&e) {
                    warn!("Error loading state: {:#}, clearing state.", e);
                }
                Self::new(cache_dir.to_owned(), release_version.to_owned())
            }
        }
    }

    /// Saves the updater state to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cache_dir).context("create_dir_all")?;
        let path = Path::new(&self.cache_dir).join("state.json");
        let file = File::create(path).context("File::create for state.json")?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self)?;
        Ok(())
    }

    fn patch_info_at(&self, index: usize) -> Option<PatchInfo> {
        if index >= self.slots.len() {
            return None;
        }
        let slot = &self.slots[index];
        // to_str only ever fails if the path is invalid utf8, which should
        // never happen, but this way we don't crash if it is.
        Some(PatchInfo {
            path: self.patch_path_for_index(index),
            number: slot.patch_number,
        })
    }

    /// This is the current patch that is running.
    /// Will be None if:
    /// - There was no good patch at time of boot.
    /// - The updater has been initialized but no boot recorded yet.
    pub fn current_boot_patch(&self) -> Option<PatchInfo> {
        if let Some(slot_index) = self.current_boot_slot_index {
            return self.patch_info_at(slot_index);
        }
        None
    }

    /// This is the patch that will be used for the next boot.
    /// Will be None if:
    /// - There has never been a patch selected.
    /// - There was a patch selected but it was later marked as bad.
    pub fn next_boot_patch(&self) -> Option<PatchInfo> {
        if let Some(slot_index) = self.next_boot_slot_index {
            return self.patch_info_at(slot_index);
        }
        None
    }

    fn validate(&mut self) -> anyhow::Result<()> {
        // iterate through all slots:
        // Make sure they're still valid.
        // If not, remove them.
        let slot_count = self.slots.len();
        let mut needs_save = false;
        // Iterate backwards so we can remove slots.
        for i in (0..slot_count).rev() {
            let slot = &self.slots[i];
            if !self.validate_slot(slot) {
                warn!("Slot {} is invalid, clearing.", i);
                self.clear_slot(i)?;
                needs_save = true;
            }
        }
        if needs_save {
            self.save()?;
        }
        Ok(())
    }

    fn validate_slot(&self, slot: &Slot) -> bool {
        // Check if the patch is known bad.
        if self.is_known_bad_patch(slot.patch_number) {
            debug!("Slot {:?} is known bad.", slot);
            return false;
        }
        let index = self
            .slots
            .iter()
            .position(|s| s.patch_number == slot.patch_number);
        let patch_path = self.patch_path_for_index(index.unwrap());
        if !patch_path.exists() {
            debug!("Slot {:?} {} does not exist.", slot, patch_path.display());
            return false;
        }
        // TODO: This should also check if the hash matches?
        // let hash = compute_hash(&PathBuf::from(&slot.path));
        // if let Ok(hash) = hash {
        //     if hash == slot.hash {
        //         return true;
        //     }
        //     error!("Hash mismatch for slot: {:?}", slot);
        // }
        true
    }

    fn latest_bootable_slot(&self) -> Option<usize> {
        // Find the latest slot that has a patch that is not bad.
        // Sort the slots by patch number, then return the highest
        // patch number that is not bad.
        let mut slots = self.slots.clone();
        slots.sort_by(|a, b| a.patch_number.cmp(&b.patch_number));
        slots.reverse();
        for slot in slots {
            if self.validate_slot(&slot) {
                return Some(slot.patch_number);
            }
        }
        None
    }

    pub fn activate_latest_bootable_patch(&mut self) -> Result<(), UpdateError> {
        self.set_next_boot_patch_slot(self.latest_bootable_slot());
        self.save().map_err(|_| UpdateError::FailedToSaveState)
    }

    fn available_slot(&self) -> usize {
        // Assume we only use two slots and pick the one that's not current.
        if self.slots.is_empty() {
            return 0;
        }
        if let Some(slot_index) = self.current_boot_slot_index {
            // This does not check next_boot_slot_index, we're assuming that
            // whoever is calling this is OK with replacing the next boot
            // patch.
            if slot_index == 0 {
                return 1;
            }
        }
        return 0;
    }

    fn clear_slot(&mut self, index: usize) -> anyhow::Result<()> {
        // Index is outside of the slots we have.
        if index >= self.slots.len() {
            // Ignore slots past the end for now?
            return Ok(());
        }
        self.slots[index] = Slot::default();
        let slot_dir_string = self.slot_dir_for_index(index);
        if slot_dir_string.exists() {
            std::fs::remove_dir_all(&slot_dir_string)?;
        }
        Ok(())
    }

    fn set_slot(&mut self, index: usize, slot: Slot) {
        debug!("Setting slot {} to {:?}", index, slot);
        if self.slots.len() < index + 1 {
            // Make sure we're not filling with empty slots.
            assert!(self.slots.len() == index);
            self.slots.resize(index + 1, Slot::default());
        }
        // Set the given slot to the given version.
        self.slots[index] = slot
    }

    fn patch_path_for_index(&self, index: usize) -> PathBuf {
        self.slot_dir_for_index(index).join("dlc.vmcode")
    }

    fn slot_dir_for_index(&self, index: usize) -> PathBuf {
        Path::new(&self.cache_dir).join(format!("slot_{}", index))
    }

    pub fn install_patch(&mut self, patch: PatchInfo) -> anyhow::Result<()> {
        let slot_index = self.available_slot();
        let slot_dir_string = self.slot_dir_for_index(slot_index);
        let slot_dir = PathBuf::from(&slot_dir_string);

        // Clear the slot.
        self.clear_slot(slot_index)?; // Invalidate the slot.
        self.save()?;
        std::fs::create_dir_all(&slot_dir)?;

        if self.is_known_bad_patch(patch.number) {
            return Err(UpdateError::InvalidArgument(
                "patch".to_owned(),
                format!("Refusing to install known bad patch: {:?}", patch),
            )
            .into());
        }

        // Move the artifact into the slot.
        let artifact_path = slot_dir.join("dlc.vmcode");
        std::fs::rename(&patch.path, &artifact_path)?;

        // Update the state to include the new slot.
        self.set_slot(
            slot_index,
            Slot {
                patch_number: patch.number,
            },
        );
        self.set_next_boot_patch_slot(Some(slot_index));

        if let Some(latest) = self.latest_patch_number() {
            if patch.number < latest {
                warn!(
                    "Installed patch {} but latest downloaded patch is {:?}",
                    patch.number, latest
                );
            }
        }
        self.save()?;

        let path = self.patch_path_for_index(slot_index);
        if !path.exists() {
            warn!(
                "Patch {} installed but does not exist {:?}",
                patch.number, path
            );
        } else {
            debug!("Patch {} installed to {:?}", patch.number, path);
        }

        Ok(())
    }

    /// Sets the current_boot slot to the next_boot slot.
    pub fn activate_current_patch(&mut self) -> Result<(), UpdateError> {
        if self.next_boot_slot_index.is_none() {
            return Err(UpdateError::InvalidState(
                "No patch to activate.".to_owned(),
            ));
        }
        self.current_boot_slot_index = self.next_boot_slot_index.clone();
        assert!(self.current_boot_slot_index.is_some());
        Ok(())
    }

    /// Switches the next boot slot to the given slot or clears it if None.
    pub fn set_next_boot_patch_slot(&mut self, maybe_index: Option<usize>) {
        self.next_boot_slot_index = maybe_index;
    }

    /// Returns highest patch number that has been installed for this release.
    /// This should represent the latest patch we still have on disk so as
    /// to prevent re-downloading patches we already have.
    /// This should essentially be the max of the patch number in the slots
    /// and the bad patch list (we don't need to keep bad patches on disk
    /// to know that they're bad).
    /// Used by the patch check logic.
    pub fn latest_patch_number(&self) -> Option<usize> {
        // Get the max of the patch numbers in the slots.
        // We probably could do this with chain and max?
        let installed_max = self.slots.iter().map(|s| s.patch_number).max();
        let failed_max = self.failed_patches.clone().into_iter().max();
        match installed_max {
            None => failed_max,
            Some(installed) => match failed_max {
                None => return installed_max,
                Some(failed) => Some(std::cmp::max(installed, failed)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use tempdir::TempDir;

    use crate::cache::{PatchInfo, UpdaterState};

    fn test_state(tmp_dir: &TempDir) -> UpdaterState {
        let cache_dir = tmp_dir.path();
        UpdaterState::new(cache_dir.to_owned(), "1.0.0+1".to_string())
    }

    fn fake_patch(tmp_dir: &TempDir, number: usize) -> super::PatchInfo {
        let path = tmp_dir.path().join(format!("patch_{}", number));
        std::fs::write(&path, "fake patch").unwrap();
        PatchInfo { number, path }
    }

    #[test]
    fn next_boot_patch_does_not_crash() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut state = test_state(&tmp_dir);
        assert_eq!(state.next_boot_patch(), None);
        state.next_boot_slot_index = Some(3);
        assert_eq!(state.next_boot_patch(), None);
        state.slots.push(super::Slot::default());
        // This used to crash, where index was bad, but slots were not empty.
        assert_eq!(state.next_boot_patch(), None);
    }

    #[test]
    fn release_version_changed() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut state = test_state(&tmp_dir);
        state.next_boot_slot_index = Some(1);
        state.save().unwrap();
        let loaded = UpdaterState::load_or_new_on_error(&state.cache_dir, &state.release_version);
        assert_eq!(loaded.next_boot_slot_index, Some(1));

        let loaded_after_version_change =
            UpdaterState::load_or_new_on_error(&state.cache_dir, "1.0.0+2");
        assert_eq!(loaded_after_version_change.next_boot_slot_index, None);
    }

    #[test]
    fn latest_downloaded_patch() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut state = test_state(&tmp_dir);
        assert_eq!(state.latest_patch_number(), None);
        state.install_patch(fake_patch(&tmp_dir, 1)).unwrap();
        assert_eq!(state.latest_patch_number(), Some(1));
        state.install_patch(fake_patch(&tmp_dir, 2)).unwrap();
        assert_eq!(state.latest_patch_number(), Some(2));
        state.install_patch(fake_patch(&tmp_dir, 1)).unwrap();
        // This probably should be Some(2) assuming we didn't write
        // over the top of patch 2 when re-installing patch 1.
        // I expect if we support rollbacks we might be more explicit
        // that it's a rollback?
        assert_eq!(state.latest_patch_number(), Some(1));
    }

    #[test]
    fn do_not_install_known_bad_patch() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut state = test_state(&tmp_dir);
        let bad_patch = fake_patch(&tmp_dir, 1);
        state.mark_patch_as_bad(bad_patch.number).unwrap();
        let number = bad_patch.number;
        assert!(state.install_patch(bad_patch).is_err());

        // Calling a second time should not error.
        state.mark_patch_as_bad(number).unwrap();
    }

    #[test]
    fn do_not_mark_bad_patch_good() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut state = test_state(&tmp_dir);
        let bad_patch = fake_patch(&tmp_dir, 1);
        assert!(state.mark_patch_as_bad(bad_patch.number).is_ok());
        assert!(state.mark_patch_as_good(bad_patch.number).is_err());
        assert!(state.is_known_bad_patch(bad_patch.number));
        assert!(!state.is_known_good_patch(bad_patch.number));
    }

    #[test]
    fn mark_patch_as_good() {
        let tmp_dir = TempDir::new("example").unwrap();
        let mut state = test_state(&tmp_dir);
        let patch = fake_patch(&tmp_dir, 1);
        state.mark_patch_as_good(patch.number).unwrap();
        assert!(state.is_known_good_patch(patch.number));
        assert!(!state.is_known_bad_patch(patch.number));
        // Marking it twice doesn't change anything.
        state.mark_patch_as_good(patch.number).unwrap();
        assert!(state.is_known_good_patch(patch.number));
        assert!(!state.is_known_bad_patch(patch.number));
    }

    #[test]
    fn is_file_not_found_test() {
        use anyhow::Context;
        assert!(!super::is_file_not_found(&anyhow::anyhow!("")));
        let tmp_dir = TempDir::new("example").unwrap();
        let path = tmp_dir.path().join("does_not_exist");
        let result = std::fs::File::open(&path).context("foo");
        assert!(result.is_err());
        assert!(super::is_file_not_found(&result.unwrap_err()));
    }

    #[test]
    fn creates_updater_state_with_client_id() {
        let tmp_dir = TempDir::new("example").unwrap();
        let state = UpdaterState::load_or_new_on_error(&tmp_dir.path().to_path_buf(), "1.0.0+1");
        assert!(state.client_id.is_some());
    }

    #[test]
    fn adds_client_id_to_saved_state() {
        let tmp_dir = TempDir::new("example").unwrap();
        let state: UpdaterState = UpdaterState {
            cache_dir: tmp_dir.path().to_path_buf(),
            release_version: "1.0.0+1".to_string(),
            client_id: None,
            current_boot_slot_index: None,
            next_boot_slot_index: None,
            failed_patches: Vec::new(),
            successful_patches: Vec::new(),
            slots: Vec::new(),
        };

        state.save().unwrap();

        let loaded_state =
            UpdaterState::load_or_new_on_error(&state.cache_dir, &state.release_version);
        assert!(loaded_state.client_id.is_some());
    }
}
