use super::{disk_manager, PatchInfo};
use anyhow::{bail, Context, Ok, Result};
use core::fmt::Debug;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PATCHES_DIR_NAME: &str = "patches";
const PATCHES_STATE_FILE_NAME: &str = "patches_state.json";

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
enum PatchBootStatus {
    /// We have successfully booted from this patch before.
    Succeeded,

    /// This patch has failed to boot before.
    Failed,

    /// We have not yet attempted to boot from this patch.
    Unknown,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
struct PatchMetadata {
    number: usize,
    size: u64,
}

/// What gets serialized to disk
#[derive(Debug, Default, Deserialize, Serialize)]
struct PatchesState {
    /// The patch we are currently running, if any.
    current_patch: Option<PatchMetadata>,

    /// The patch that will be run on the next app boot, if any. This may be the same
    /// as the current patch if no new patch has been downloaded.
    next_boot_patch: Option<PatchMetadata>,

    /// The highest patch number we have seen. This may be higher than the current or next
    /// patch if we downloaded a patch that failed to boot.
    highest_seen_patch_number: Option<usize>,
}

/// Abstracts the patch file system structure
/// TBD whether this trait is actually needed or if we can just use the PatchManager
/// struct directly. Having it would allow us to mock PatchManager, but it is (in theory)
/// simple enough that we could just use the real thing.
pub trait ManagePatches {
    /// Copies the patch file at file_path to the manager's directory structure sets
    /// this patch as the next patch to boot.
    fn add_patch(&mut self, number: usize, file_path: &Path) -> Result<()>;

    /// Returns the currently running patch, or None if no patch is installed.
    fn get_current_patch(&self) -> Option<PatchInfo>;

    /// Returns the next patch to boot, or None if no new patch has been downloaded.
    fn get_next_boot_patch(&self) -> Option<PatchInfo>;

    /// Records that the patch with number patch_number booted successfully and is
    /// safe to use for future boots.
    fn record_boot_success_for_patch(&mut self, patch_number: usize) -> Result<()>;

    /// Records that the patch with number patch_number failed to boot, and ensures
    /// that it will never be returned as the next or current patch.
    fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()>;

    /// The highest patch number that has been added. This may be higher than the
    /// current or next patch if we downloaded a patch that failed to boot.
    fn highest_seen_patch_number(&self) -> Option<usize>;

    /// Resets the patch manager to its initial state, removing all patches. This is
    /// intended to be used when a new release version is installed.
    fn reset(&mut self) -> Result<()>;
}

impl Debug for dyn ManagePatches {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "TODO")
    }
}

#[derive(Debug)]
pub struct PatchManager {
    root_dir: PathBuf,
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
        disk_manager::read(&path).ok()
    }

    fn save_patches_state(&self) -> Result<()> {
        let path = self.root_dir.join(PATCHES_STATE_FILE_NAME);
        disk_manager::write(&self.patches_state, &path)
    }

    fn patch_artifacts_dir(&self) -> PathBuf {
        self.root_dir.join(PATCHES_DIR_NAME)
    }

    fn dir_for_patch_number(&self, patch_number: usize) -> PathBuf {
        self.patch_artifacts_dir().join(patch_number.to_string())
    }

    /// Patch artifacts are stored in the patches directory, with the name <patch_number>.vmcode
    fn file_path_for_patch_number(&self, patch_number: usize) -> PathBuf {
        self.dir_for_patch_number(patch_number).join("dlc.vmcode")
    }

    fn patch_info_for_number(&self, patch_number: usize) -> PatchInfo {
        PatchInfo {
            path: self.file_path_for_patch_number(patch_number),
            number: patch_number,
        }
    }

    /// Checks that the patch with the given number:
    ///   - Has metadata associated with it
    ///   - Has not previously failed to boot
    ///   - Has an artifact on disk
    ///   - That artifact on disk is the same size it was when it was installed
    ///
    /// Returns Ok if the patch is bootable, or an error if it is not.
    fn validate_patch_is_bootable(&self, patch: &PatchMetadata) -> Result<()> {
        // if patch.boot_status == PatchBootStatus::Failed {
        //     bail!(format!(
        //         "Patch {} has previously failed to boot, cannot boot from it",
        //         patch.number
        //     ));
        // }

        let artifact_path = self.file_path_for_patch_number(patch.number);
        if !Path::exists(&artifact_path) {
            bail!(format!(
                "Patch {} does not exist at {}",
                patch.number,
                artifact_path.display()
            ));
        }

        let artifact_size_on_disk = std::fs::metadata(&artifact_path)?.len();
        if artifact_size_on_disk != patch.size {
            bail!(format!(
                "Patch {} has size {} on disk, but expected size {}",
                patch.number, artifact_size_on_disk, patch.size
            ));
        }

        Ok(())
    }

    fn delete_patch_artifacts(&mut self, patch_number: usize) -> Result<()> {
        if let Some(patch) = self.get_next_boot_patch() {
            if patch.number == patch_number {
                bail!("Cannot remove next patch");
            }
        }

        let patch_dir = self.file_path_for_patch_number(patch_number);

        std::fs::remove_dir_all(&patch_dir).context(format!(
            "Failed to delete patch dir {}",
            &patch_dir.display()
        ))?;

        // TODO

        self.save_patches_state()
    }
}

impl ManagePatches for PatchManager {
    fn add_patch(&mut self, patch_number: usize, file_path: &Path) -> Result<()> {
        // if self.patches_state.patch_for_number(patch_number).is_some() {
        //     // TODO: verify that this patch isn't one we already know about.
        //     bail!("Patch {} already exists in add_patch", patch_number,);
        // }

        let patch_path = self.file_path_for_patch_number(patch_number);

        std::fs::create_dir_all(self.dir_for_patch_number(patch_number))
            .with_context(|| format!("create_dir_all failed for {}", patch_path.display()))?;

        std::fs::rename(file_path, &patch_path)?;

        let new_patch = PatchMetadata {
            number: patch_number,
            size: std::fs::metadata(&patch_path)?.len(),
        };
        self.patches_state.next_boot_patch = Some(new_patch);
        self.patches_state.highest_seen_patch_number = Some(patch_number);
        self.save_patches_state()
    }

    fn get_current_patch(&self) -> Option<PatchInfo> {
        self.patches_state
            .current_patch
            .map(|patch| self.patch_info_for_number(patch.number))
    }

    fn get_next_boot_patch(&self) -> Option<PatchInfo> {
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

        if let Some(current_patch) = self.patches_state.current_patch {
            self.delete_patch_artifacts(current_patch.number)?;
        }

        self.patches_state.current_patch = Some(next_boot_patch);
        self.save_patches_state()
    }

    fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()> {
        let next_boot_patch = self
            .patches_state
            .next_boot_patch
            .context("No next_boot_patch")?;

        if next_boot_patch.number != patch_number {
            bail!(
                "Attempted to record boot failure for patch {} but next_boot_patch is {}",
                patch_number,
                next_boot_patch.number
            );
        }

        self.patches_state.next_boot_patch = self.patches_state.current_patch;

        self.save_patches_state()
    }

    // fn set_next_patch_to_latest_bootable(&mut self) -> Result<()> {
    //     self.latest_bootable_patch_number()
    //         .map(|patch_number| self.set_next_boot_patch(patch_number))
    //         .unwrap_or_else(|| {
    //             bail!("No bootable patches found, cannot set_next_patch_to_latest_bootable")
    //         })
    // }

    // fn set_current_patch_to_next(&mut self) -> Result<()> {
    //     if self.patches_state.next_boot_patch_number.is_some() {
    //         self.patches_state.current_patch_number = self.patches_state.next_boot_patch_number;
    //         self.save_patches_state()
    //     } else {
    //         bail!("Cannot record_booted_from_next_patch because there is no next patch");
    //     }
    // }

    // fn is_known_good_patch(&self, patch_number: usize) -> bool {
    //     self.patches_state
    //         .known_good_patch_numbers()
    //         .contains(&patch_number)
    // }

    // fn is_known_bad_patch(&self, patch_number: usize) -> bool {
    //     self.patches_state
    //         .known_good_patch_numbers()
    //         .contains(&patch_number)
    // }

    fn highest_seen_patch_number(&self) -> Option<usize> {
        self.patches_state.highest_seen_patch_number
    }
    // fn latest_patch_number(&self) -> Option<usize> {
    //     self.patches_state.patches.iter().map(|p| p.number).max()
    // }

    fn reset(&mut self) -> Result<()> {
        self.patches_state = PatchesState::default();
        self.save_patches_state()?;
        std::fs::remove_dir_all(self.patch_artifacts_dir()).context(format!(
            "Failed to delete patches dir {}",
            self.patch_artifacts_dir().display()
        ))
    }
}

#[cfg(test)]
mod manage_patch_tests {
    use tempdir::TempDir;

    use super::*;

    #[test]
    fn get_current_patch_returns_none_if_no_patch_installed() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let manager = PatchManager::with_root_dir(temp_dir.path().to_path_buf());
        assert!(manager.get_current_patch().is_none());
    }

    #[test]
    fn can_get_and_set_current_patch() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let manager = PatchManager::with_root_dir(temp_dir.path().to_path_buf());
        let patch = PatchInfo {
            path: PathBuf::from("asdf"),
            number: 1,
        };

        // assert_eq!(manager.get_current_patch().);
    }

    #[test]
    fn get_next_boot_patch_returns_none_if_no_patch_downloaded() {
        todo!()
    }

    #[test]
    fn get_next_boot_patch_returns_patch_info_if_patch_downloaded() {
        todo!()
    }
}
