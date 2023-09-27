use super::{disk_manager, PatchInfo};
use anyhow::{bail, Context, Ok, Result};
use core::fmt::Debug;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

const PATCHES_DIR_NAME: &str = "patches";
const PATCHES_STATE_FILE_NAME: &str = "patches_state.json";

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

    /// Records that the patch with number patch_number booted successfully.
    fn mark_patch_as_good(&mut self, patch_number: usize) -> Result<()>;

    /// Records that the patch with number patch_number failed to boot, and ensures
    /// that it will never be returned as the next or current patch.
    fn mark_patch_as_bad(&mut self, patch_number: usize) -> Result<()>;

    /// Whether this patch has been successfully booted from before.
    fn is_known_good_patch(&self, patch_number: usize) -> bool;

    /// Whether this patch has failed to boot before.
    fn is_known_bad_patch(&self, patch_number: usize) -> bool;

    /// Sets the next patch to boot to the latest known good patch.
    fn set_next_patch_to_latest_bootable(&mut self) -> Result<()>;

    /// Called when the next patch to boot has been booted from successfully.
    /// This updates the "current patch" number to the "next patch" number.
    fn record_booted_from_next_patch(&mut self) -> Result<()>;

    /// The highest patch number (good or bad) that we know about.
    fn latest_patch_number(&self) -> Option<usize>;

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

/// What gets serialized to disk
#[derive(Debug, Default, Deserialize, Serialize)]
struct PatchesState {
    current_patch_number: Option<usize>,
    next_boot_patch_number: Option<usize>,
    known_good_patch_numbers: HashSet<usize>,
    known_bad_patch_numbers: HashSet<usize>,
    all_patches: HashSet<usize>,
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

    fn patches_dir(&self) -> PathBuf {
        self.root_dir.join(PATCHES_DIR_NAME)
    }

    fn path_for_patch_number(&self, patch_number: usize) -> PathBuf {
        self.root_dir
            .join(PATCHES_DIR_NAME)
            .join(format!("{}.vmcode", patch_number))
    }

    fn set_next_boot_patch(&mut self, patch_number: usize) -> Result<()> {
        self.patches_state.next_boot_patch_number = Some(patch_number);
        Ok(())
    }

    fn patch_info_for_number(&self, patch_number: usize) -> PatchInfo {
        PatchInfo {
            path: self.path_for_patch_number(patch_number),
            number: patch_number,
        }
    }
}

impl ManagePatches for PatchManager {
    fn add_patch(&mut self, patch_number: usize, file_path: &Path) -> Result<()> {
        if self.patches_state.all_patches.contains(&patch_number) {
            // TODO: verify that this patch isn't one we already know about.
            bail!(format!(
                "Patch {} already exists in add_patch",
                patch_number,
            ));
        }

        let patch_path = self.path_for_patch_number(patch_number);

        std::fs::create_dir_all(self.patches_dir())
            .with_context(|| format!("create_dir_all failed for {}", patch_path.display()))?;

        std::fs::rename(file_path, patch_path)?;

        self.patches_state.all_patches.insert(patch_number);
        self.set_next_boot_patch(patch_number)?;
        self.save_patches_state()
    }

    fn get_current_patch(&self) -> Option<PatchInfo> {
        self.patches_state
            .current_patch_number
            .map(|number| self.patch_info_for_number(number))
    }

    fn get_next_boot_patch(&self) -> Option<PatchInfo> {
        self.patches_state
            .next_boot_patch_number
            .map(|number| self.patch_info_for_number(number))
    }

    fn mark_patch_as_good(&mut self, patch_number: usize) -> Result<()> {
        if self
            .patches_state
            .known_bad_patch_numbers
            .contains(&patch_number)
        {
            // This patch has been marked as bad, this shouldn't happen.
            bail!(format!(
                "Cannot mark patch {} as good because it was previously marked as bad",
                patch_number
            ));
        }

        self.patches_state
            .known_good_patch_numbers
            .insert(patch_number);

        self.save_patches_state()?;

        Ok(())
    }

    fn mark_patch_as_bad(&mut self, patch_number: usize) -> Result<()> {
        if self
            .patches_state
            .known_good_patch_numbers
            .contains(&patch_number)
        {
            // If we have previously marked this as a good patch, remove it from the
            // known good set. This might happen if the patch file was changed on disk
            // or fails to boot for some other reason.
            self.patches_state
                .known_good_patch_numbers
                .remove(&patch_number);
        }

        self.patches_state
            .known_bad_patch_numbers
            .insert(patch_number);

        self.save_patches_state()?;

        Ok(())
    }

    fn set_next_patch_to_latest_bootable(&mut self) -> Result<()> {
        // TODO
        self.save_patches_state()
    }

    fn record_booted_from_next_patch(&mut self) -> Result<()> {
        if self.patches_state.next_boot_patch_number.is_some() {
            self.patches_state.current_patch_number = self.patches_state.next_boot_patch_number;
            self.save_patches_state()
        } else {
            bail!("Cannot record_booted_from_next_patch because there is no next patch");
        }
    }

    fn is_known_good_patch(&self, patch_number: usize) -> bool {
        self.patches_state
            .known_good_patch_numbers
            .contains(&patch_number)
    }

    fn is_known_bad_patch(&self, patch_number: usize) -> bool {
        self.patches_state
            .known_bad_patch_numbers
            .contains(&patch_number)
    }

    fn latest_patch_number(&self) -> Option<usize> {
        self.patches_state.all_patches.iter().max().copied()
    }

    fn reset(&mut self) -> Result<()> {
        self.patches_state = PatchesState::default();
        self.save_patches_state()?;
        std::fs::remove_dir_all(self.patches_dir()).context(format!(
            "Failed to delete patches dir {}",
            self.patches_dir().display()
        ))
    }
}

#[cfg(test)]
mod init_tests {
    use std::path::Path;

    use tempdir::TempDir;

    use super::*;

    // #[test]
    // fn with_root_dir_errs_if_cant_create_dir() {
    //     // Attemt to initialize with a bogus path that cannot be created.
    //     assert!(PatchManager::with_root_dir(PathBuf::from("/../asdf")).is_err())
    // }

    // #[test]
    // fn with_root_dir_creates_dir_if_not_exists() {
    //     let temp_dir = TempDir::new("patch_manager").unwrap();
    //     let temp_dir_path = temp_dir.path();
    //     std::fs::remove_dir(temp_dir_path).unwrap();

    //     // Verify that we've removed the path.
    //     assert!(!Path::exists(temp_dir_path));
    //     assert!(PatchManager::with_root_dir(temp_dir_path.to_path_buf()).is_ok());
    //     // PatchManager::with_root_dir should have created the path we gave it.
    //     assert!(Path::exists(temp_dir_path));
    // }
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
