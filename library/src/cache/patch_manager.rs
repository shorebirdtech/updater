use super::{disk_io, PatchInfo};
use anyhow::{bail, Context, Ok, Result};
use core::fmt::Debug;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(test)]
use mockall::automock;
#[cfg(test)]
use tempdir::TempDir;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as error}; // Workaround to use println! for logs.

const PATCHES_DIR_NAME: &str = "patches";
const PATCHES_STATE_FILE_NAME: &str = "patches_state.json";

#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
struct PatchMetadata {
    /// The number of the patch.
    number: usize,

    /// The size of the patch artifact on disk.
    size: u64,
}

/// What gets serialized to disk
#[derive(Debug, Default, Deserialize, Serialize)]
struct PatchesState {
    /// The patch we are currently running, if any.
    last_booted_patch: Option<PatchMetadata>,

    /// The patch that will be run on the next app boot, if any. This may be the same
    /// as the last booted patch patch if no new patch has been downloaded.
    next_boot_patch: Option<PatchMetadata>,

    /// The highest patch number we have seen. This may be higher than the last booted
    /// patch or next patch if we downloaded a patch that failed to boot.
    highest_seen_patch_number: Option<usize>,
}

/// Abstracts the process of managing patches.
#[cfg_attr(test, automock)]
pub trait ManagePatches {
    /// Copies the patch file at file_path to the manager's directory structure sets
    /// this patch as the next patch to boot.
    fn add_patch(&mut self, number: usize, file_path: &Path) -> Result<()>;

    /// Returns the patch we most recently successfully booted from (usually the currently running patch),
    /// or None if no patch is installed.
    fn last_successfully_booted_patch(&self) -> Option<PatchInfo>;

    /// Returns the next patch to boot, or None if no new patch has been downloaded.
    fn get_next_boot_patch(&mut self) -> Option<PatchInfo>;

    /// Records that the patch with number patch_number booted successfully and is
    /// safe to use for future boots.
    fn record_boot_success_for_patch(&mut self, patch_number: usize) -> Result<()>;

    /// Records that the patch with number patch_number failed to boot, and ensures
    /// that it will never be returned as the next boot or last booted patch.
    fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()>;

    /// The highest patch number that has been added. This may be higher than the
    /// last booted or next boot patch if we downloaded a patch that failed to boot.
    fn highest_seen_patch_number(&self) -> Option<usize>;

    /// Resets the patch manager to its initial state, removing all patches. This is
    /// intended to be used when a new release version is installed.
    fn reset(&mut self) -> Result<()>;
}

// This allows us to use the Debug trait on dyn ManagePatches, which is
// required to have it as a property of UpdaterState.
impl Debug for dyn ManagePatches {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ManagePatches")
    }
}

#[derive(Debug)]
pub struct PatchManager {
    /// The base directory used to store patch artifacts and state.
    /// The directory structure created within this directory is:
    ///  patches_state.json
    ///  patches/
    ///    <patch_number>/
    ///      dlc.vmcode
    ///    <patch_number>/
    ///      dlc.vmcode
    root_dir: PathBuf,

    /// Metadata about the patches we have downloaded that is persisted to disk.
    patches_state: PatchesState,
}

impl PatchManager {
    /// Creates a new PatchManager with the given root directory. This directory is
    /// assumed to exist. The PatchManager will use this directory to store its
    /// state and patch binaries.
    pub fn with_root_dir(root_dir: PathBuf) -> Self {
        let patches_state = Self::load_patches_state(&root_dir).unwrap_or_default();

        Self {
            root_dir,
            patches_state,
        }
    }

    fn load_patches_state(root_dir: &Path) -> Option<PatchesState> {
        let path = root_dir.join(PATCHES_STATE_FILE_NAME);
        disk_io::read(&path).ok()
    }

    fn save_patches_state(&self) -> Result<()> {
        let path = self.root_dir.join(PATCHES_STATE_FILE_NAME);
        disk_io::write(&self.patches_state, &path)
    }

    /// The directory where all patch artifacts are stored.
    fn patch_artifacts_dir(&self) -> PathBuf {
        self.root_dir.join(PATCHES_DIR_NAME)
    }

    /// The directory where artifacts for the patch with the given number are stored.
    fn dir_for_patch_number(&self, patch_number: usize) -> PathBuf {
        self.patch_artifacts_dir().join(patch_number.to_string())
    }

    /// The path to the runnable patch artifact with the given number. Runnable patch artifact files are
    /// named <patch_number>.vmcode
    fn patch_artifact_path(&self, patch_number: usize) -> PathBuf {
        self.dir_for_patch_number(patch_number).join("dlc.vmcode")
    }

    fn patch_info_for_number(&self, patch_number: usize) -> PatchInfo {
        PatchInfo {
            path: self.patch_artifact_path(patch_number),
            number: patch_number,
        }
    }

    /// Checks that the patch with the given number:
    ///   - Has an artifact on disk
    ///   - That artifact on disk is the same size it was when it was installed
    ///
    /// Returns Ok if the patch is bootable, or an error if it is not.
    fn validate_patch_is_bootable(&self, patch: &PatchMetadata) -> Result<()> {
        let artifact_path = self.patch_artifact_path(patch.number);
        if !Path::exists(&artifact_path) {
            bail!(
                "Patch {} does not exist at {}",
                patch.number,
                artifact_path.display()
            );
        }

        let artifact_size_on_disk = std::fs::metadata(&artifact_path)?.len();
        if artifact_size_on_disk != patch.size {
            bail!(
                "Patch {} has size {} on disk, but expected size {}",
                patch.number,
                artifact_size_on_disk,
                patch.size
            );
        }

        Ok(())
    }

    fn delete_patch_artifacts(&mut self, patch_number: usize) -> Result<()> {
        info!("deleting patch artifacts for patch {}", patch_number);

        let patch_dir = self.dir_for_patch_number(patch_number);

        std::fs::remove_dir_all(&patch_dir)
            .with_context(|| format!("Failed to delete patch dir {}", &patch_dir.display()))
    }
}

impl ManagePatches for PatchManager {
    fn add_patch(&mut self, patch_number: usize, file_path: &Path) -> Result<()> {
        if !file_path.exists() {
            bail!("patch file {} does not exist", file_path.display());
        }

        let patch_path = self.patch_artifact_path(patch_number);

        std::fs::create_dir_all(self.dir_for_patch_number(patch_number))
            .with_context(|| format!("create_dir_all failed for {}", patch_path.display()))?;

        std::fs::rename(file_path, &patch_path)?;

        let new_patch = PatchMetadata {
            number: patch_number,
            size: std::fs::metadata(&patch_path)?.len(),
        };

        // If a patch was never booted (next_boot_patch != last_booted_patch), we should delete
        // it here before setting next_boot_patch to the new patch.
        if let (Some(last_boot_patch), Some(next_boot_patch)) = (
            self.patches_state.next_boot_patch,
            self.patches_state.last_booted_patch,
        ) {
            if last_boot_patch.number != next_boot_patch.number {
                if let Err(e) = self.delete_patch_artifacts(next_boot_patch.number) {
                    error!(
                        "Failed to delete patch artifacts for patch {}. Error: {}",
                        patch_number, e
                    );
                }
            }
        }

        self.patches_state.next_boot_patch = Some(new_patch);
        self.patches_state.highest_seen_patch_number = self
            .patches_state
            .highest_seen_patch_number
            .map(|highest_patch_number: usize| highest_patch_number.max(patch_number))
            .or(Some(patch_number));
        self.save_patches_state()
    }

    fn last_successfully_booted_patch(&self) -> Option<PatchInfo> {
        self.patches_state
            .last_booted_patch
            .map(|patch| self.patch_info_for_number(patch.number))
    }

    fn get_next_boot_patch(&mut self) -> Option<PatchInfo> {
        let next_boot_patch = match self.patches_state.next_boot_patch {
            Some(patch) => patch,
            None => return None,
        };

        if let Err(e) = self.validate_patch_is_bootable(&next_boot_patch) {
            error!("Patch {} is not bootable: {}", next_boot_patch.number, e);
            if let Err(e) = self.delete_patch_artifacts(next_boot_patch.number) {
                error!(
                    "Failed to delete patch artifacts for patch {}. Error: {}",
                    next_boot_patch.number, e
                );
            }
            self.patches_state.next_boot_patch = None;

            // If a previously booted patch is the same as the next boot patch, clear it.
            if let Some(current_patch) = self.patches_state.last_booted_patch {
                if current_patch.number == next_boot_patch.number {
                    if let Err(e) = self.delete_patch_artifacts(current_patch.number) {
                        error!(
                            "Failed to delete patch artifacts for patch {}. Error: {}",
                            next_boot_patch.number, e
                        );
                    }
                    self.patches_state.last_booted_patch = None;
                }
            }

            if let Err(e) = self.save_patches_state() {
                error!("Failed to save patches state: {}", e);
            }
            return None;
        }

        self.patches_state
            .next_boot_patch
            .as_ref()
            .map(|patch| self.patch_info_for_number(patch.number))
    }

    fn record_boot_success_for_patch(&mut self, patch_number: usize) -> Result<()> {
        let next_boot_patch = self
            .patches_state
            .next_boot_patch
            .context("No next_boot_patch")?;

        if next_boot_patch.number != patch_number {
            bail!(
                "Attempted to record boot success for patch {} but next_boot_patch is {}",
                patch_number,
                next_boot_patch.number
            );
        }

        if let Some(current_patch) = self.patches_state.last_booted_patch {
            self.delete_patch_artifacts(current_patch.number)?;
        }

        self.patches_state.last_booted_patch = Some(next_boot_patch);
        self.save_patches_state()
    }

    fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()> {
        let next_boot_patch = self
            .patches_state
            .next_boot_patch
            .context("No next_boot_patch")?;

        if next_boot_patch.number != patch_number {
            bail!(
                "Attempted to record boot failure for patch {} but should have booted from {}",
                patch_number,
                next_boot_patch.number
            );
        }

        if let Err(e) = self.delete_patch_artifacts(next_boot_patch.number) {
            error!(
                "Failed to delete patch artifacts for patch {}. Error: {}",
                patch_number, e
            );
        }
        self.patches_state.next_boot_patch = None;

        if let Some(current_patch) = self.patches_state.last_booted_patch {
            if current_patch.number == patch_number {
                self.patches_state.last_booted_patch = None;
            }
        }
        self.save_patches_state()
    }

    fn highest_seen_patch_number(&self) -> Option<usize> {
        self.patches_state.highest_seen_patch_number
    }

    fn reset(&mut self) -> Result<()> {
        self.patches_state = PatchesState::default();
        self.save_patches_state()?;
        std::fs::remove_dir_all(self.patch_artifacts_dir()).with_context(|| {
            format!(
                "Failed to delete patches dir {}",
                self.patch_artifacts_dir().display()
            )
        })
    }
}

#[cfg(test)]
impl PatchManager {
    pub fn manager_for_test(temp_dir: &TempDir) -> PatchManager {
        PatchManager::with_root_dir(temp_dir.path().to_owned())
    }

    pub fn add_patch_for_test(&mut self, temp_dir: &TempDir, patch_number: usize) -> Result<()> {
        let file_path = &temp_dir
            .path()
            .join(format!("patch{}.vmcode", patch_number));
        std::fs::write(file_path, patch_number.to_string().repeat(patch_number)).unwrap();
        self.add_patch(patch_number, file_path)
    }
}

#[cfg(test)]
mod add_patch_tests {
    use super::*;
    use std::path::Path;
    use tempdir::TempDir;

    #[test]
    fn errs_if_file_path_does_not_exist() {
        let mut manager = PatchManager::manager_for_test(&TempDir::new("patch_manager").unwrap());
        assert!(manager
            .add_patch(1, Path::new("/path/to/file/that/does/not/exist"))
            .is_err());
    }

    #[test]
    fn adds_patch_successfully() {
        let patch_number = 1;
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents).unwrap();

        assert!(manager
            .add_patch(patch_number, Path::new(file_path))
            .is_ok());

        assert_eq!(
            manager.patches_state.next_boot_patch,
            Some(PatchMetadata {
                number: patch_number,
                size: patch_file_contents.len() as u64
            })
        );
        assert!(!file_path.exists());
        assert_eq!(manager.highest_seen_patch_number(), Some(patch_number));
    }

    #[test]
    fn does_not_set_higher_highest_seen_patch_number_if_added_patch_is_lower() -> Result<()> {
        let patch_file_contents = "patch contents";

        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        assert!(manager.highest_seen_patch_number().is_none());

        // Add patch 1
        let file_path = &temp_dir.path().join("patch.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(1, file_path).is_ok());
        assert_eq!(manager.highest_seen_patch_number(), Some(1));

        // Add patch 4, expect 4 to be the highest patch number we've seen
        let file_path = &temp_dir.path().join("patch.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(4, file_path).is_ok());
        assert_eq!(manager.highest_seen_patch_number(), Some(4));

        // Add patch 3, expect 4 to still be the highest patch number we've seen
        let file_path = &temp_dir.path().join("patch.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(3, file_path).is_ok());
        assert_eq!(manager.highest_seen_patch_number(), Some(4));

        Ok(())
    }
}

#[cfg(test)]
mod last_successfully_booted_patch_tests {
    use super::*;
    use tempdir::TempDir;

    #[test]
    fn returns_none_if_no_patch_has_been_booted() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;
        assert!(manager.last_successfully_booted_patch().is_none());

        Ok(())
    }

    #[test]
    fn returns_value_from_patches_state() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;

        let expected = PatchInfo {
            path: manager.patch_artifact_path(1),
            number: 1,
        };
        manager.patches_state.last_booted_patch = manager.patches_state.next_boot_patch;
        assert_eq!(manager.last_successfully_booted_patch(), Some(expected));

        Ok(())
    }
}

#[cfg(test)]
mod get_next_boot_patch_tests {
    use super::*;
    use anyhow::Result;
    use tempdir::TempDir;

    #[test]
    fn returns_none_if_no_next_boot_patch() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        assert!(manager.get_next_boot_patch().is_none());
    }

    #[test]
    fn returns_none_if_next_boot_patch_is_not_bootable() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;

        // Write junk to the artifact, this should render the patch unbootable in the eyes
        // of the PatchManager.
        let artifact_path = manager.patch_artifact_path(1);
        std::fs::write(&artifact_path, "junk")?;

        assert!(manager.get_next_boot_patch().is_none());

        // Ensure the internal state is cleared.
        assert_eq!(manager.patches_state.next_boot_patch, None);

        // The artifact should have been deleted.
        assert!(!&artifact_path.exists());

        Ok(())
    }

    #[test]
    fn clears_current_and_next_on_boot_failure_if_they_are_the_same() -> Result<()> {
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(1, file_path).is_ok());

        // Write junk to the artifact, this should render the patch unbootable in the eyes
        // of the PatchManager.
        let artifact_path = manager.patch_artifact_path(1);
        std::fs::write(&artifact_path, "junk")?;

        assert!(manager.get_next_boot_patch().is_none());

        // Ensure the internal state is cleared.
        assert_eq!(manager.patches_state.next_boot_patch, None);
        assert_eq!(manager.patches_state.last_booted_patch, None);

        // The artifact should have been deleted.
        assert!(!&artifact_path.exists());

        Ok(())
    }
}

#[cfg(test)]
mod record_boot_success_for_patch_tests {
    use super::*;
    use anyhow::{Ok, Result};
    use tempdir::TempDir;

    #[test]
    fn errs_if_no_next_boot_patch() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        // This should fail because no patches have been added.
        assert!(manager.record_boot_success_for_patch(1).is_err());

        Ok(())
    }

    #[test]
    fn errs_if_patch_number_does_not_match_next_patch() -> Result<()> {
        let patch_number = 1;
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(patch_number, file_path).is_ok());

        Ok(())
    }

    #[test]
    fn succeeds_when_provided_next_boot_patch_number() -> Result<()> {
        let patch_number = 1;
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(patch_number, file_path).is_ok());

        assert!(manager.record_boot_success_for_patch(patch_number).is_ok());

        Ok(())
    }
}

#[cfg(test)]
mod record_boot_failure_for_patch_tests {
    use super::*;
    use anyhow::{Ok, Result};
    use tempdir::TempDir;

    #[test]
    fn errs_if_no_next_boot_patch() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        assert!(manager.record_boot_failure_for_patch(1).is_err());

        Ok(())
    }

    #[test]
    fn errs_if_patch_number_does_not_match_next_boot_patch() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;

        assert!(manager.record_boot_failure_for_patch(2).is_err());

        Ok(())
    }

    #[test]
    fn deletes_failed_patch_artifacts() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;
        assert!(manager.record_boot_success_for_patch(1).is_ok());
        let succeeded_patch_artifact_path = manager.patch_artifact_path(1);

        manager.add_patch_for_test(&temp_dir, 2)?;
        let failed_patch_artifact_path = manager.patch_artifact_path(2);

        // Make sure patch artifacts exist
        assert!(failed_patch_artifact_path.exists());
        assert!(succeeded_patch_artifact_path.exists());

        assert!(manager.record_boot_failure_for_patch(2).is_ok());
        assert!(!failed_patch_artifact_path.exists());

        Ok(())
    }

    #[test]
    fn clears_last_booted_patch_if_it_is_the_failed_patch() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;
        let patch_artifact_path = manager.patch_artifact_path(1);

        // Pretend we booted from this patch
        assert!(manager.record_boot_success_for_patch(1).is_ok());
        assert_eq!(manager.last_successfully_booted_patch().unwrap().number, 1);
        assert_eq!(manager.get_next_boot_patch().unwrap().number, 1);
        assert!(patch_artifact_path.exists());

        // Now pretend it failed to boot
        assert!(manager.record_boot_failure_for_patch(1).is_ok());
        assert!(manager.last_successfully_booted_patch().is_none());
        assert!(manager.get_next_boot_patch().is_none());
        assert!(!patch_artifact_path.exists());

        Ok(())
    }
}

#[cfg(test)]
mod highest_seen_patch_number_tests {
    use super::*;
    use anyhow::{Ok, Result};
    use tempdir::TempDir;

    #[test]
    fn returns_value_from_internal_state() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        assert!(manager.patches_state.highest_seen_patch_number.is_none());
        assert!(manager.highest_seen_patch_number().is_none());

        manager.patches_state.highest_seen_patch_number = Some(1);
        assert_eq!(manager.highest_seen_patch_number(), Some(1));

        Ok(())
    }
}

#[cfg(test)]
mod reset_tests {
    use super::*;
    use anyhow::{Ok, Result};
    use tempdir::TempDir;

    #[test]
    fn deletes_patches_dir_and_resets_patches_state() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;
        let path_artifacts_dir = manager.patch_artifacts_dir();

        // Make sure the directory and artifact files were created
        assert!(path_artifacts_dir.exists());
        assert_eq!(std::fs::read_dir(&path_artifacts_dir).unwrap().count(), 1);

        assert!(manager.reset().is_ok());

        // Make sure the directory and artifact files were deleted
        assert!(!path_artifacts_dir.exists());

        Ok(())
    }
}
