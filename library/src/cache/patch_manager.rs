use super::{disk_io, PatchInfo};
use anyhow::{bail, Context, Result};
use base64::Engine;
use core::fmt::Debug;
use ring::signature;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(test)]
use mockall::automock;
#[cfg(test)]
use tempdir::TempDir;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as error, println as debug}; // Workaround to use println! for logs.

const PATCHES_DIR_NAME: &str = "patches";
const PATCHES_STATE_FILE_NAME: &str = "patches_state.json";
const PATCH_ARTIFACT_FILENAME: &str = "dlc.vmcode";

// This is no longer Copy-able because of the hash and signature fields. This
// change results in us adding clone() calls to PatchMetadata in a several
// places below.
// #[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
struct PatchMetadata {
    /// The number of the patch.
    number: usize,

    /// The size of the patch artifact on disk.
    size: u64,

    /// The hash of the inflated patch
    hash: String,

    /// The base64-encoded signature of the hash
    signature: String,
}

/// What gets serialized to disk
#[derive(Debug, Default, Deserialize, Serialize)]
struct PatchesState {
    /// The patch we are currently running, if any.
    last_booted_patch: Option<PatchMetadata>,

    /// The last patch we attempted to boot, if any.
    last_attempted_patch: Option<PatchMetadata>,

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
    fn add_patch(&mut self, number: usize, file_path: &Path, patch_hash: &str) -> Result<()>;

    /// Returns the patch we most recently successfully booted from (usually the currently running patch),
    /// or None if no patch is installed.
    fn last_successfully_booted_patch(&self) -> Option<PatchInfo>;

    /// The patch we most recently attempted to boot. Will be the same as
    /// last_successfully_booted_patch if the last boot was successful.
    fn last_attempted_boot_patch(&self) -> Option<PatchInfo>;

    /// Returns the next patch to boot, or None if:
    /// - no patches have been downloaded
    /// - we cannot boot from the patch(es) on disk
    fn next_boot_patch(&mut self) -> Option<PatchInfo>;

    /// Record that we're booting. If we have a next path, updates the last
    /// attempted patch to be the next boot patch.
    fn record_boot_start_for_patch(&mut self, patch_number: usize) -> Result<()>;

    /// Marks last_attempted_patch as "good", updates last_booted_patch to be the same,
    /// and deletes all patch artifacts older than the last_booted_patch.
    fn record_boot_success(&mut self) -> Result<()>;

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
        match disk_io::read(&path) {
            Ok(maybe_state) => maybe_state,
            Err(e) => {
                debug!(
                    "Failed to load patches state from {}: {}",
                    path.display(),
                    e
                );
                None
            }
        }
    }

    fn save_patches_state(&self) -> Result<()> {
        let path = self.root_dir.join(PATCHES_STATE_FILE_NAME);
        disk_io::write(&self.patches_state, &path)
    }

    /// The directory where all patch artifacts are stored.
    fn patches_dir(&self) -> PathBuf {
        self.root_dir.join(PATCHES_DIR_NAME)
    }

    /// The directory where artifacts for the patch with the given number are stored.
    fn patch_dir(&self, patch_number: usize) -> PathBuf {
        self.patches_dir().join(patch_number.to_string())
    }

    /// The path to the runnable patch artifact with the given number. Runnable patch artifact files are
    /// named <patch_number>.vmcode
    fn patch_artifact_path(&self, patch_number: usize) -> PathBuf {
        self.patch_dir(patch_number).join(PATCH_ARTIFACT_FILENAME)
    }

    fn patch_info_for_number(&self, patch_number: usize) -> PatchInfo {
        PatchInfo {
            path: self.patch_artifact_path(patch_number),
            number: patch_number,
            hash: "asdf".to_owned(),
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

        // If the last boot we tried was this patch, make sure we succeeded or the patch is bad.
        if self.is_patch_last_attempted_patch(patch.number) {
            // We are trying to boot from the same patch that we tried to boot from last time.

            match self.last_successful_boot_patch_number() {
                Some(last_successful_patch_number)
                    if last_successful_patch_number == patch.number =>
                {
                    // Our last boot attempt was this patch, and we've successfully booted from this
                    // patch before.  This patch is safe to boot from.
                }
                _ => {
                    // We've tried to boot from this patch before and didn't
                    // succeed. Don't try again.
                    bail!(
                        "Already attempted and failed to boot patch {}",
                        patch.number
                    )
                }
            }
        }

        // Ensure patch signature is valid for recorded hash

        // public.pem
        let public_key_base_64_str = "MIIBCgKCAQEA2wdpEGbuvlPsb9i0qYrfMefJnEw1BHTi8SYZTKrXOvJWmEpPE1hWfbkvYzXu5a96gV1yocF3DMwn04VmRlKhC4AhsD0NL0UNhYhotbKG91Kwi1vAXpHhCdz5gQEBw0K1uB4Jz+zK6WK+31PryYpwLwbyXNqXoY8IAAUQ4STsHYV5w+BMSi8pepWMRd7DR9RHcbNOZlJvdBQ5NxvB4JN4dRMq8cC73ez1P9d7Dfwv3TWY+he9EmuXLT2UivZSlHIrGBa7MFfqyUe2ro0F7Te/B0si12itBbWIqycvqcXjeOPNn6WEpqN7IWjb9LUh162JyYaz5Lb/VeeJX8LKtElccwIDAQAB";
        let public_key_str = base64::prelude::BASE64_STANDARD
            .decode(public_key_base_64_str)
            .unwrap();

        info!("generating public key from {:?}", public_key_str);
        let public_key = signature::UnparsedPublicKey::new(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            public_key_str,
        );
        info!("public key is {:?}", public_key);
        info!("signature is {}", patch.signature);
        let decoded_sig = match base64::prelude::BASE64_STANDARD.decode(patch.signature.clone()) {
            Ok(sig) => sig,
            Err(e) => {
                error!("Failed to decode signature: {:?}", e);
                vec![]
            }
        };

        info!("decoded signature is {:?}", decoded_sig);
        info!("verifying signature...");
        match public_key.verify(patch.hash.as_bytes(), &decoded_sig) {
            Ok(_) => {
                info!("Signature is valid");
            }
            Err(e) => {
                error!("Signature is invalid: {:?}", e);
            }
        }

        use sha2::{Digest, Sha256}; // `Digest` is needed for `Sha256::new()`;

        let path = self.patch_artifact_path(patch.number);
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher)?;
        // Check that the length from copy is the same as the file size?
        let hash = hasher.finalize();
        info!("patch hash is {}", patch.hash);
        info!("hash digest is {}", hex::encode(hash));

        info!("hashes match? {}", hex::encode(hash) == patch.hash);

        Ok(())
    }

    /// Whether the given patch number is the last one we attempted to boot
    /// (whether it was successful or not).
    fn is_patch_last_attempted_patch(&self, patch_number: usize) -> bool {
        self.patches_state
            .last_attempted_patch
            .as_ref()
            .map(|patch| patch.number == patch_number)
            .unwrap_or(false)
    }

    /// The number of the patch we last successfully booted, if any.
    fn last_successful_boot_patch_number(&self) -> Option<usize> {
        self.patches_state
            .last_booted_patch
            .as_ref()
            .map(|patch| patch.number)
    }

    fn delete_patch_artifacts(&mut self, patch_number: usize) -> Result<()> {
        info!("Deleting patch artifacts for patch {}", patch_number);

        let patch_dir = self.patch_dir(patch_number);

        std::fs::remove_dir_all(&patch_dir)
            .map_err(|e| {
                error!("Failed to delete patch dir {}: {}", patch_dir.display(), e);
                e
            })
            .with_context(|| format!("Failed to delete patch dir {}", &patch_dir.display()))
    }

    /// Deletes artifacts for the provided bad_patch_number and attempts to set the next_boot_patch to the last
    /// successfully booted patch. If the last successfully booted patch is not bootable or has the same number
    /// as the patch we're falling back from, we clear it as well.
    fn try_fall_back_from_patch(&mut self, bad_patch_number: usize) {
        // No need to log failure – delete_patch_artifacts logs for us.
        let _ = self.delete_patch_artifacts(bad_patch_number);

        if let Some(ref next_boot_patch) = self.patches_state.next_boot_patch {
            // If our next boot patch is bad_patch_number, clear it.
            if next_boot_patch.number == bad_patch_number {
                self.patches_state.next_boot_patch = None;
            }
        }

        // If we think we can still boot from the last booted patch, set it as the next_boot_patch.
        // If something happened to render the last boot patch unbootable, clear it and delete its artifacts.
        if let Some(last_boot_patch) = self.patches_state.last_booted_patch.clone() {
            if last_boot_patch.number != bad_patch_number
                && self.validate_patch_is_bootable(&last_boot_patch).is_ok()
            {
                self.patches_state.next_boot_patch = Some(last_boot_patch);
            } else {
                self.patches_state.last_booted_patch = None;
                // No need to log failure – delete_patch_artifacts logs for us.
                let _ = self.delete_patch_artifacts(last_boot_patch.number);
            }
        }
    }

    /// Deletes all patch artifacts with numbers less than patch_number.
    /// We intentionally only delete older patch artifacts. Consider the case:
    ///
    /// 1. We start booting patch 2
    /// 2. While booting (i.e., in between boot start and boot success), we download and inflate patch 3
    /// 3. We finish booting patch 2
    ///
    /// Deleting all other patch artifacts would delete patch 3, and because we've "seen" patch 3,
    /// we would never try to download it again (it would be considered "bad").
    fn delete_patch_artifacts_older_than(&mut self, patch_number: usize) -> Result<()> {
        for entry in std::fs::read_dir(self.patches_dir())? {
            let entry = entry?;
            match entry.file_name().to_string_lossy().parse::<usize>() {
                Ok(number) if number < patch_number => {
                    // delete_patch_artifacts logs for us, no need to log here.
                    let _ = self.delete_patch_artifacts(number);
                }
                Ok(_) => {}
                Err(e) => {
                    error!(
                        "Failed to parse patch number from patches directory entry, deleting: {}",
                        e
                    );
                    // Attempt to delete the unrecognized directory, but don't stop
                    // the artifact deletion process if it fails.
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }

        Ok(())
    }
}

impl ManagePatches for PatchManager {
    fn add_patch(&mut self, patch_number: usize, file_path: &Path, patch_hash: &str) -> Result<()> {
        if !file_path.exists() {
            bail!("Patch file {} does not exist", file_path.display());
        }

        let patch_path = self.patch_artifact_path(patch_number);

        std::fs::create_dir_all(self.patch_dir(patch_number))
            .with_context(|| format!("create_dir_all failed for {}", patch_path.display()))?;

        std::fs::rename(file_path, &patch_path)?;

        let new_patch = PatchMetadata {
            number: patch_number,
            size: std::fs::metadata(&patch_path)?.len(),
            hash: patch_hash.to_owned(),
            signature: "replace_me".to_owned(),
        };

        // If a patch was never booted (next_boot_patch != last_booted_patch), we should delete
        // it here before setting next_boot_patch to the new patch.
        if let (Some(last_boot_patch), Some(next_boot_patch)) = (
            self.patches_state.next_boot_patch.clone(),
            self.patches_state.last_booted_patch.clone(),
        ) {
            if last_boot_patch.number != next_boot_patch.number {
                let _ = self.delete_patch_artifacts(next_boot_patch.number);
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
            .as_ref()
            .map(|patch| self.patch_info_for_number(patch.number))
    }

    fn last_attempted_boot_patch(&self) -> Option<PatchInfo> {
        self.patches_state
            .last_attempted_patch
            .as_ref()
            .map(|patch| self.patch_info_for_number(patch.number))
    }

    fn next_boot_patch(&mut self) -> Option<PatchInfo> {
        let next_boot_patch = match self.patches_state.next_boot_patch.clone() {
            Some(patch) => patch,
            None => return None,
        };

        if let Err(e) = self.validate_patch_is_bootable(&next_boot_patch) {
            error!("Patch {} is not bootable: {}", next_boot_patch.number, e);

            self.try_fall_back_from_patch(next_boot_patch.number);

            if let Err(e) = self.save_patches_state() {
                error!("Failed to save patches state: {}", e);
            }
        }

        self.patches_state
            .next_boot_patch
            .as_ref()
            .map(|patch| self.patch_info_for_number(patch.number))
    }

    fn record_boot_start_for_patch(&mut self, patch_number: usize) -> Result<()> {
        let next_boot_patch = self
            .patches_state
            .next_boot_patch
            .clone()
            .context("No next_boot_patch")?;

        if next_boot_patch.number != patch_number {
            bail!(
                "Attempted to record boot success for patch {} but next_boot_patch is {}",
                patch_number,
                next_boot_patch.number
            );
        }

        self.patches_state.last_attempted_patch = Some(next_boot_patch);
        self.save_patches_state()
    }

    fn record_boot_success(&mut self) -> Result<()> {
        let boot_patch = self
            .patches_state
            .last_attempted_patch
            .clone()
            .context("No last_attempted_patch")?;

        self.patches_state.last_booted_patch = Some(boot_patch.clone());
        if let Err(e) = self.delete_patch_artifacts_older_than(boot_patch.number) {
            error!(
                "Failed to delete patch artifacts older than {}: {}",
                boot_patch.number, e
            );
        }
        self.save_patches_state()
    }

    fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()> {
        self.try_fall_back_from_patch(patch_number);
        self.save_patches_state()
    }

    fn highest_seen_patch_number(&self) -> Option<usize> {
        self.patches_state.highest_seen_patch_number
    }

    fn reset(&mut self) -> Result<()> {
        self.patches_state = PatchesState::default();
        self.save_patches_state()?;
        std::fs::remove_dir_all(self.patches_dir()).with_context(|| {
            format!(
                "Failed to delete patches dir {}",
                self.patches_dir().display()
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
        self.add_patch(patch_number, file_path, "asdf")
    }
}

#[cfg(test)]
mod debug_tests {
    use tempdir::TempDir;

    use super::PatchManager;

    #[test]
    fn manage_patches_is_debug() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let patch_manager: Box<dyn super::ManagePatches> = Box::new(
            super::PatchManager::with_root_dir(temp_dir.path().to_owned()),
        );
        assert_eq!(format!("{:?}", patch_manager), "ManagePatches");
    }

    #[test]
    fn patch_manager_is_debug() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let patch_manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        let expected_str = format!(
            "PatchManager {{ root_dir: \"{}\", patches_state: PatchesState {{ last_booted_patch: None, last_attempted_patch: None, next_boot_patch: None, highest_seen_patch_number: None }} }}",
            temp_dir.path().display()
        );
        assert_eq!(format!("{:?}", patch_manager), expected_str);
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
            .add_patch(1, Path::new("/path/to/file/that/does/not/exist"), "asdf")
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
            .add_patch(patch_number, Path::new(file_path), "asdf")
            .is_ok());

        assert_eq!(
            manager.patches_state.next_boot_patch,
            Some(PatchMetadata {
                number: patch_number,
                size: patch_file_contents.len() as u64,
                hash: "asdf".to_owned(),
                signature: "replace_me".to_owned(),
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
        assert!(manager.add_patch(1, file_path, "asdf").is_ok());
        assert_eq!(manager.highest_seen_patch_number(), Some(1));

        // Add patch 4, expect 4 to be the highest patch number we've seen
        let file_path = &temp_dir.path().join("patch.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(4, file_path, "asdf").is_ok());
        assert_eq!(manager.highest_seen_patch_number(), Some(4));

        // Add patch 3, expect 4 to still be the highest patch number we've seen
        let file_path = &temp_dir.path().join("patch.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(3, file_path, "asdf").is_ok());
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
            hash: "asdf".to_string(),
        };
        manager.patches_state.last_booted_patch = manager.patches_state.next_boot_patch.clone();
        assert_eq!(manager.last_successfully_booted_patch(), Some(expected));

        Ok(())
    }
}

#[cfg(test)]
mod next_boot_patch_tests {
    use super::*;
    use anyhow::Result;
    use tempdir::TempDir;

    #[test]
    fn returns_none_if_no_next_boot_patch() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        assert!(manager.next_boot_patch().is_none());
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

        assert!(manager.next_boot_patch().is_none());

        // Ensure the internal state is cleared.
        assert!(manager.patches_state.next_boot_patch.is_none());

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
        assert!(manager.add_patch(1, file_path, "asdf").is_ok());

        // Write junk to the artifact, this should render the patch unbootable in the eyes
        // of the PatchManager.
        let artifact_path = manager.patch_artifact_path(1);
        std::fs::write(&artifact_path, "junk")?;

        assert!(manager.next_boot_patch().is_none());

        // Ensure the internal state is cleared.
        assert!(manager.patches_state.next_boot_patch.is_none());
        assert!(manager.patches_state.last_booted_patch.is_none());

        // The artifact should have been deleted.
        assert!(!&artifact_path.exists());

        Ok(())
    }

    #[test]
    fn falls_back_to_last_booted_patch_if_still_bootable() -> Result<()> {
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;

        // Add patch 1, pretend it booted successfully.
        assert!(manager.add_patch(1, file_path, "asdf").is_ok());
        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_success().is_ok());

        // Add patch 2, pretend it failed to boot.
        let file_path = &temp_dir.path().join("patch2.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(2, file_path, "asdf").is_ok());
        assert!(manager.record_boot_start_for_patch(2).is_ok());
        assert!(manager.record_boot_failure_for_patch(2).is_ok());

        // Verify that we will next attempt to boot from patch 1.
        assert_eq!(manager.next_boot_patch().unwrap().number, 1);

        Ok(())
    }

    #[test]
    fn does_not_fall_back_to_last_booted_patch_if_corrupted() -> Result<()> {
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;

        // Add patch 1, pretend it booted successfully.
        assert!(manager.add_patch(1, file_path, "asdf").is_ok());
        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_success().is_ok());

        // Add patch 2, pretend it failed to boot.
        let file_path = &temp_dir.path().join("patch2.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(2, file_path, "asdf").is_ok());
        assert!(manager.record_boot_start_for_patch(2).is_ok());
        assert!(manager.record_boot_failure_for_patch(2).is_ok());

        // Write junk to patch 1's artifact. This should prevent us from falling back to it.
        let patch_1_artifact_path = manager.patch_artifact_path(1);
        std::fs::write(patch_1_artifact_path, "junk")?;

        // Verify that we will not attempt to boot from either patch.
        assert!(manager.next_boot_patch().is_none());

        Ok(())
    }

    #[test]
    fn returns_null_patch_if_first_patch_failed_to_boot() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Add a first patch and pretend it failed to boot.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_failure_for_patch(1)?;

        // Because there is no previous patch, we should not attempt to boot any patch.
        assert!(manager.next_boot_patch().is_none());

        Ok(())
    }

    #[test]
    fn returns_last_booted_patch_if_next_patch_failed_to_boot() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Add a first patch and pretend it booted successfully.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_success()?;

        // Add a second patch and pretend it failed to boot.
        manager.add_patch_for_test(&temp_dir, 2)?;
        manager.record_boot_start_for_patch(2)?;
        manager.record_boot_failure_for_patch(2)?;

        // Verify that we will next attempt to boot from patch 1.
        assert_eq!(manager.next_boot_patch().unwrap().number, 1);

        Ok(())
    }

    #[test]
    fn returns_null_if_first_patch_did_not_successfully_boot() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Add a first patch and pretend it booted successfully.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;

        assert!(manager.next_boot_patch().is_none());

        Ok(())
    }

    #[test]
    fn returns_null_if_next_patch_did_not_successfully_boot() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Add a first patch and pretend it booted successfully.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_success()?;

        manager.add_patch_for_test(&temp_dir, 2)?;
        manager.record_boot_start_for_patch(2)?;

        assert!(manager
            .next_boot_patch()
            .is_some_and(|patch| patch.number == 1));

        Ok(())
    }
}

#[cfg(test)]
mod fall_back_tests {
    use super::*;

    #[test]
    fn does_nothing_if_no_patch_exists() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        assert!(manager.patches_state.last_booted_patch.is_none());
        assert!(manager.patches_state.next_boot_patch.is_none());

        manager.try_fall_back_from_patch(1);

        assert!(manager.patches_state.last_booted_patch.is_none());
        assert!(manager.patches_state.next_boot_patch.is_none());

        Ok(())
    }

    #[test]
    fn sets_next_patch_to_latest_patch_if_no_next_patch_exists() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        assert!(manager.patches_state.next_boot_patch.is_none());

        manager.patches_state.last_booted_patch = Some(PatchMetadata {
            number: 1,
            size: 1,
            hash: "asdf".to_owned(),
            signature: "replace_me".to_owned(),
        });
        manager.try_fall_back_from_patch(1);

        assert_eq!(
            manager.patches_state.next_boot_patch,
            manager.patches_state.last_booted_patch
        );

        Ok(())
    }

    #[test]
    fn sets_next_patch_to_latest_patch_if_both_are_present() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        // Download and successfully boot from patch 1
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_success()?;

        // Download and fall back from patch 2
        manager.add_patch_for_test(&temp_dir, 2)?;

        manager.try_fall_back_from_patch(2);

        assert_eq!(manager.patches_state.last_booted_patch.unwrap().number, 1);
        assert_eq!(manager.patches_state.next_boot_patch.unwrap().number, 1);

        Ok(())
    }

    #[test]
    fn clears_next_and_last_patches_if_both_fail_validation() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        // Download and successfully boot from patch 1, and then corrupt it on disk.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_success()?;
        let patch_1_path = manager.patch_artifact_path(1);
        std::fs::write(patch_1_path, "junkjunkjunk")?;

        // Download and fall back from patch 2
        manager.add_patch_for_test(&temp_dir, 2)?;

        manager.try_fall_back_from_patch(2);

        // Neither patch should exist.
        assert!(manager.patches_state.last_booted_patch.is_none());
        assert!(manager.patches_state.next_boot_patch.is_none());

        Ok(())
    }

    #[test]
    fn does_not_clear_next_patch_if_changed_since_boot_start() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        // Simulate a situation where we download both patches 1 and 2.
        manager.add_patch_for_test(&temp_dir, 1)?;

        // Start booting from patch 1.
        manager.record_boot_start_for_patch(1)?;

        // Download patch 2 before patch 1 finishes booting.
        manager.add_patch_for_test(&temp_dir, 2)?;

        manager.record_boot_failure_for_patch(1)?;

        manager.try_fall_back_from_patch(1);

        assert!(manager.patches_state.last_booted_patch.is_none());
        assert_eq!(manager.patches_state.next_boot_patch.unwrap().number, 2);

        Ok(())
    }

    #[test]
    fn succeeds_if_deleting_artifacts_fails() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        // Download and successfully boot from patch 1, and then corrupt it on disk.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_success()?;

        // Download patch 2.
        manager.add_patch_for_test(&temp_dir, 2)?;

        // Remove all data for both patches.
        let patch_dir = manager.patch_dir(1);
        std::fs::remove_dir_all(patch_dir)?;
        let patch_dir = manager.patch_dir(2);
        std::fs::remove_dir_all(patch_dir)?;

        manager.try_fall_back_from_patch(2);

        assert!(manager.patches_state.last_booted_patch.is_none());
        assert!(manager.patches_state.next_boot_patch.is_none());

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
        assert!(manager.record_boot_success().is_err());

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
        assert!(manager.add_patch(patch_number, file_path, "asdf").is_ok());
        assert!(manager.record_boot_success().is_err());

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
        assert!(manager.add_patch(patch_number, file_path, "asdf").is_ok());

        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_success().is_ok());

        Ok(())
    }

    #[test]
    fn repeated_calls_to_record_success_succeed() -> Result<()> {
        let patch_number = 1;
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;

        // Add the patch, make sure it has an artifact.
        assert!(manager.add_patch(patch_number, file_path, "asdf").is_ok());
        let patch_artifact_path = manager.patch_artifact_path(patch_number);
        assert!(patch_artifact_path.exists());

        // Record success, make sure the artifact still exists.
        manager.record_boot_start_for_patch(patch_number)?;
        assert!(manager.record_boot_success().is_ok());
        assert_eq!(
            manager.last_successfully_booted_patch().unwrap().number,
            patch_number
        );
        assert_eq!(manager.next_boot_patch().unwrap().number, patch_number);
        assert!(patch_artifact_path.exists());

        // Record another success, make sure the artifact still exists.
        assert!(manager.record_boot_success().is_ok());
        assert_eq!(
            manager.last_successfully_booted_patch().unwrap().number,
            patch_number
        );
        assert_eq!(manager.next_boot_patch().unwrap().number, patch_number);
        assert!(patch_artifact_path.exists());

        Ok(())
    }

    #[test]
    fn deletes_other_patch_artifacts() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        // Download patches 1, 2, and 3 before we start booting from patch 2.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.add_patch_for_test(&temp_dir, 2)?;
        manager.add_patch_for_test(&temp_dir, 3)?;

        // Start booting from our latest patch.
        manager.record_boot_start_for_patch(3)?;

        // Download patch 4 while we're booting from patch 3.
        manager.add_patch_for_test(&temp_dir, 4)?;

        // Record success for patch 3, make sure the artifact still exists.
        manager.record_boot_success()?;

        // Make sure that recording success for patch 2 deleted artifacts for prior
        // patches but not for subsequent patches.
        let mut patch_dir_names = std::fs::read_dir(manager.patches_dir())?
            .map(|res| res.map(|e| e.path()))
            .map(|e| e.unwrap())
            .map(|e| e.file_name().unwrap().to_owned())
            .collect::<Vec<_>>();
        patch_dir_names.sort();
        assert_eq!(patch_dir_names, vec!["3", "4"]);

        Ok(())
    }

    #[test]
    fn deletes_unrecognized_directories_in_patches_dir() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::with_root_dir(temp_dir.path().to_owned());

        // Add a junk directory to the patches directory.
        let junk_dir = manager.patches_dir().join("junk");
        std::fs::create_dir_all(&junk_dir)?;

        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.add_patch_for_test(&temp_dir, 2)?;
        manager.record_boot_start_for_patch(2)?;
        manager.record_boot_success()?;

        assert!(!junk_dir.exists());
        assert!(!manager.patch_dir(1).exists());

        Ok(())
    }
}

#[cfg(test)]
mod record_boot_failure_for_patch_tests {
    use super::*;
    use anyhow::{Ok, Result};
    use tempdir::TempDir;

    #[test]
    fn deletes_failed_patch_artifacts() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;
        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_success().is_ok());
        let succeeded_patch_artifact_path = manager.patch_artifact_path(1);

        manager.add_patch_for_test(&temp_dir, 2)?;
        let failed_patch_artifact_path = manager.patch_artifact_path(2);

        // Make sure patch artifacts exist
        assert!(failed_patch_artifact_path.exists());
        assert!(succeeded_patch_artifact_path.exists());

        assert!(manager.record_boot_start_for_patch(2).is_ok());
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
        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_success().is_ok());
        assert_eq!(manager.last_successfully_booted_patch().unwrap().number, 1);
        assert_eq!(manager.next_boot_patch().unwrap().number, 1);
        assert!(patch_artifact_path.exists());

        // Now pretend it failed to boot
        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_failure_for_patch(1).is_ok());
        assert!(manager.last_successfully_booted_patch().is_none());
        assert!(manager.next_boot_patch().is_none());
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
        let path_artifacts_dir = manager.patches_dir();

        // Make sure the directory and artifact files were created
        assert!(path_artifacts_dir.exists());
        assert_eq!(std::fs::read_dir(&path_artifacts_dir).unwrap().count(), 1);

        assert!(manager.reset().is_ok());

        // Make sure the directory and artifact files were deleted
        assert!(!path_artifacts_dir.exists());

        Ok(())
    }
}
