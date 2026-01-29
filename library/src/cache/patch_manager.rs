use super::{disk_io, signing, PatchInfo};
use crate::yaml::PatchVerificationMode;
use anyhow::{bail, Context, Result};
use core::fmt::Debug;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[cfg(test)]
use mockall::automock;
#[cfg(test)]
use tempdir::TempDir;

const PATCHES_DIR_NAME: &str = "patches";
const PATCHES_STATE_FILE_NAME: &str = "patches_state.json";
const PATCH_ARTIFACT_FILENAME: &str = "dlc.vmcode";

/// Information about a patch that is persisted to disk.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
struct PatchMetadata {
    /// The number of the patch.
    number: usize,

    /// The size of the patch artifact on disk.
    size: u64,

    /// The hash of the patch artifact on disk.
    hash: String,

    /// The signature of `hash`.
    signature: Option<String>,
}

/// What gets serialized to disk
#[derive(Debug, Default, Deserialize, Serialize)]
struct PatchesState {
    /// The patch we are currently running, if any.
    last_booted_patch: Option<PatchMetadata>,

    /// The patch that will be run on the next app boot, if any. This may be the same
    /// as the last booted patch patch if no new patch has been downloaded.
    next_boot_patch: Option<PatchMetadata>,

    /// This is given a value when we start booting a patch (record_boot_start_for_patch) and is
    /// cleared when:
    ///  - the patch boots successfully (record_boot_success)
    ///  - the patch fails to boot (record_boot_failure_for_patch)
    ///  - the system initializes (on_init, we take this to mean the patch failed to boot)
    currently_booting_patch: Option<PatchMetadata>,

    /// A list of patch numbers that we have tried and failed to install.
    /// We should never attempt to download or install these again for the
    /// current release.
    known_bad_patches: HashSet<usize>,
}

/// Abstracts the storage of patches on disk.
///
/// The implementation of this (PatchManager) should only be responsible for
/// translating what is on disk into a form that is useful for the updater and
/// vice versa. Some business logic has crept in in the form of validation, and
/// we should consider moving that into a separate module.
#[cfg_attr(test, automock)]
pub trait ManagePatches {
    /// Copies the patch file at file_path to the manager's directory structure
    /// sets this patch as the next patch to boot.
    ///
    /// The explicit lifetime is required for automock to work with Options.
    /// See https://github.com/asomers/mockall/issues/61.
    #[allow(clippy::needless_lifetimes)]
    fn add_patch<'a>(
        &mut self,
        number: usize,
        file_path: &Path,
        hash: &str,
        signature: Option<&'a str>,
    ) -> Result<()>;

    /// Returns the patch we most recently successfully booted from (usually the currently running patch),
    /// or None if no patch is installed.
    fn last_successfully_booted_patch(&self) -> Option<PatchInfo>;

    /// The patch we are currently booting, if any. This will only have a value:
    ///   1. Between record_boot_start_for_patch and record_boot_success or record_boot_failure_for_patch
    ///   2. On init if we attempted to boot a patch but never recorded a successful boot (e.g., because
    ///      the system crashed).
    fn currently_booting_patch(&self) -> Option<PatchInfo>;

    /// Returns the next patch to boot, or None if:
    /// - no patches have been downloaded
    /// - we cannot boot from the patch(es) on disk
    fn next_boot_patch(&self) -> Option<PatchInfo>;

    /// Performs integrity checks on the next boot patch and updates the state accordingly. Returns
    /// an error if the patch exists but is not bootable.
    fn validate_next_boot_patch(&mut self) -> anyhow::Result<()>;

    /// Record that we're booting. If we have a next path, updates the last
    /// attempted patch to be the next boot patch.
    fn record_boot_start_for_patch(&mut self, patch_number: usize) -> Result<()>;

    /// Marks last_attempted_patch as "good", updates last_booted_patch to be the same,
    /// and deletes all patch artifacts older than the last_booted_patch.
    fn record_boot_success(&mut self) -> Result<()>;

    /// Records that the patch with number patch_number failed to boot, and ensures
    /// that it will never be returned as the next boot or last booted patch.
    fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()>;

    /// Whether we have failed to boot from the patch with `patch_number`.
    fn is_known_bad_patch(&self, patch_number: usize) -> bool;

    /// Deletes artifacts for the provided patch_number if they exist.
    /// If the patch is the next_boot_patch, it is cleared.
    fn remove_patch(&mut self, patch_number: usize) -> Result<()>;

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

    /// The key used to sign patch hashes for the current release, if any. If this is
    /// not None, all patches must have a signature that can be verified with this key.
    patch_public_key: Option<String>,

    /// Controls when signature verification occurs: at boot time (strict) or only
    /// at install time (install_only).
    verification_mode: PatchVerificationMode,
}

impl PatchManager {
    /// Creates a new PatchManager with the given root directory. This directory is
    /// assumed to exist. The PatchManager will use this directory to store its
    /// state and patch binaries.
    pub fn new(
        root_dir: PathBuf,
        patch_public_key: Option<&str>,
        verification_mode: PatchVerificationMode,
    ) -> Self {
        let patches_state = Self::load_patches_state(&root_dir).unwrap_or_default();

        Self {
            root_dir,
            patches_state,
            patch_public_key: patch_public_key.map(|s| s.to_owned()),
            verification_mode,
        }
    }

    fn load_patches_state(root_dir: &Path) -> Option<PatchesState> {
        let path = root_dir.join(PATCHES_STATE_FILE_NAME);
        match disk_io::read(&path) {
            Ok(maybe_state) => maybe_state,
            Err(e) => {
                shorebird_debug!(
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
        }
    }

    /// Checks that the patch with the given number:
    ///   - Has an artifact on disk
    ///   - That artifact on disk is the same size it was when it was installed
    ///   - In Strict mode: verifies the signature against the hash
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

        // In Strict mode, verify the signature at boot time.
        // This ensures the patch file hasn't been tampered with since installation.
        if self.verification_mode == PatchVerificationMode::Strict {
            if let Some(public_key) = &self.patch_public_key {
                let signature = patch
                    .signature
                    .clone()
                    .context("Patch signature is missing")?;

                // Compute the hash of the patch file on disk and verify it matches.
                let patch_hash = signing::hash_file(&artifact_path)?;
                signing::check_signature(&patch_hash, &signature, public_key)?;
            } else {
                shorebird_info!("No public key provided, skipping signature verification");
            }
        }

        Ok(())
    }

    fn delete_patch_artifacts(&mut self, patch_number: usize) -> Result<()> {
        let patch_dir = self.patch_dir(patch_number);
        if !patch_dir.exists() {
            shorebird_debug!("Patch {} not installed, nothing to delete", patch_number);
            return Ok(());
        }

        shorebird_info!("Deleting patch artifacts for patch {}", patch_number);

        std::fs::remove_dir_all(&patch_dir)
            .map_err(|e| {
                shorebird_error!("Failed to delete patch dir {}: {}", patch_dir.display(), e);
                e
            })
            .with_context(|| format!("Failed to delete patch dir {}", &patch_dir.display()))
    }

    /// Deletes artifacts for the provided bad_patch_number and attempts to set the next_boot_patch to the last
    /// successfully booted patch. If the last successfully booted patch is not bootable or has the same number
    /// as the patch we're falling back from, we clear it as well.
    fn try_fall_back_from_patch(&mut self, bad_patch_number: usize) -> Result<()> {
        // Continue even if we fail to delete the patch artifacts. It's more important to not try to
        // boot from a bad patch than to delete its artifacts.
        // No need to log failure – delete_patch_artifacts logs for us.
        let _ = self.delete_patch_artifacts(bad_patch_number);

        let is_bad_patch_last_booted_patch = self
            .patches_state
            .last_booted_patch
            .clone()
            .map(|patch| patch.number == bad_patch_number)
            .unwrap_or(false);
        let is_bad_patch_next_boot_patch = self
            .patches_state
            .next_boot_patch
            .clone()
            .map(|patch| patch.number == bad_patch_number)
            .unwrap_or(false);

        if is_bad_patch_last_booted_patch && is_bad_patch_next_boot_patch {
            // If both patches are bad, delete them both and boot from the base release.
            shorebird_info!("Clearing last booted patch and next boot patch");
            self.patches_state.last_booted_patch = None;
            self.patches_state.next_boot_patch = None;
        } else if is_bad_patch_next_boot_patch {
            shorebird_info!("Clearing next boot patch");
            self.patches_state.next_boot_patch = None;

            if let Some(last_boot_patch) = self.patches_state.last_booted_patch.clone() {
                if self.validate_patch_is_bootable(&last_boot_patch).is_ok() {
                    shorebird_info!(
                        "Setting last booted patch {} as next boot patch",
                        last_boot_patch.number
                    );
                    self.patches_state.next_boot_patch = Some(last_boot_patch);
                } else {
                    shorebird_info!(
                        "Last booted patch {} is not bootable, deleting artifacts",
                        last_boot_patch.number
                    );
                    self.patches_state.last_booted_patch = None;
                    // No need to log failure – delete_patch_artifacts logs for us.
                    let _ = self.delete_patch_artifacts(last_boot_patch.number);
                }
            }
        }

        self.save_patches_state()
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
        shorebird_info!("Deleting patch artifacts older than {}", patch_number);
        for entry in std::fs::read_dir(self.patches_dir())? {
            let entry = entry?;
            match entry.file_name().to_string_lossy().parse::<usize>() {
                Ok(number) if number < patch_number => {
                    // delete_patch_artifacts logs for us, no need to log here.
                    let _ = self.delete_patch_artifacts(number);
                }
                Ok(_) => {}
                Err(e) => {
                    shorebird_error!(
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
    // The explicit lifetime is required for automock to work with Options.
    // See https://github.com/asomers/mockall/issues/61.
    #[allow(clippy::needless_lifetimes)]
    fn add_patch<'a>(
        &mut self,
        patch_number: usize,
        file_path: &Path,
        hash: &str,
        signature: Option<&'a str>,
    ) -> Result<()> {
        if !file_path.exists() {
            bail!("Patch file {} does not exist", file_path.display());
        }

        // In InstallOnly mode, verify signature at install time.
        // In Strict mode, signature verification happens at boot time instead.
        if self.verification_mode == PatchVerificationMode::InstallOnly {
            if let Some(public_key) = &self.patch_public_key {
                let sig = signature.context("Patch signature is missing")?;
                signing::check_signature(hash, sig, public_key)?;
            }
        }

        let patch_path = self.patch_artifact_path(patch_number);

        std::fs::create_dir_all(self.patch_dir(patch_number))
            .with_context(|| format!("create_dir_all failed for {}", patch_path.display()))?;

        std::fs::rename(file_path, &patch_path)?;

        let new_patch = PatchMetadata {
            number: patch_number,
            size: std::fs::metadata(&patch_path)?.len(),
            hash: hash.to_owned(),
            signature: signature.map(|s| s.to_owned()),
        };

        // If a patch was never booted (next_boot_patch != last_booted_patch), we should delete
        // it here before setting next_boot_patch to the new patch.
        if let (Some(last_boot_patch), Some(next_boot_patch)) = (
            self.patches_state.last_booted_patch.clone(),
            self.patches_state.next_boot_patch.clone(),
        ) {
            if last_boot_patch.number != next_boot_patch.number {
                shorebird_info!(
                    "Patch {} was installed but never booted never booted, deleting artifacts",
                    next_boot_patch.number
                );
                let _ = self.delete_patch_artifacts(next_boot_patch.number);
            }
        }

        self.patches_state.next_boot_patch = Some(new_patch);
        self.save_patches_state()
    }

    fn last_successfully_booted_patch(&self) -> Option<PatchInfo> {
        self.patches_state
            .last_booted_patch
            .as_ref()
            .map(|patch| self.patch_info_for_number(patch.number))
    }

    fn currently_booting_patch(&self) -> Option<PatchInfo> {
        self.patches_state
            .currently_booting_patch
            .as_ref()
            .map(|patch| self.patch_info_for_number(patch.number))
    }

    fn validate_next_boot_patch(&mut self) -> anyhow::Result<()> {
        let next_boot_patch = match self.patches_state.next_boot_patch.clone() {
            Some(patch) => patch,
            None => return anyhow::Ok(()),
        };

        shorebird_info!("Validating patch {}", next_boot_patch.number);

        if let Err(e) = self.validate_patch_is_bootable(&next_boot_patch) {
            shorebird_error!("Patch {} is not bootable: {}", next_boot_patch.number, e);

            if let Err(e) = self.try_fall_back_from_patch(next_boot_patch.number) {
                shorebird_error!(
                    "Failed to fall back from next_boot_patch {}: {}",
                    next_boot_patch.number,
                    e
                );
            }

            return Err(e);
        }

        anyhow::Ok(())
    }

    fn next_boot_patch(&self) -> Option<PatchInfo> {
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

        self.patches_state.currently_booting_patch = Some(next_boot_patch.clone());
        self.save_patches_state()
    }

    fn record_boot_success(&mut self) -> Result<()> {
        let boot_patch = self
            .patches_state
            .currently_booting_patch
            .clone()
            .context("No currently_booting_patch")?;

        self.patches_state.currently_booting_patch = None;
        self.patches_state.last_booted_patch = Some(boot_patch.clone());
        if let Err(e) = self.delete_patch_artifacts_older_than(boot_patch.number) {
            shorebird_error!(
                "Failed to delete patch artifacts older than {}: {}",
                boot_patch.number,
                e
            );
        }
        self.save_patches_state()
    }

    fn record_boot_failure_for_patch(&mut self, patch_number: usize) -> Result<()> {
        self.patches_state.currently_booting_patch = None;
        self.patches_state.known_bad_patches.insert(patch_number);
        self.try_fall_back_from_patch(patch_number)
    }

    fn is_known_bad_patch(&self, patch_number: usize) -> bool {
        self.patches_state.known_bad_patches.contains(&patch_number)
    }

    fn remove_patch(&mut self, patch_number: usize) -> Result<()> {
        self.try_fall_back_from_patch(patch_number)
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
        PatchManager::new(
            temp_dir.path().to_owned(),
            None,
            PatchVerificationMode::default(),
        )
    }

    pub fn add_patch_for_test(&mut self, temp_dir: &TempDir, patch_number: usize) -> Result<()> {
        self.add_signed_patch_for_test(temp_dir, patch_number, "hash", None)
    }

    pub fn add_signed_patch_for_test(
        &mut self,
        temp_dir: &TempDir,
        patch_number: usize,
        hash: &str,
        signature: Option<&str>,
    ) -> Result<()> {
        let file_path = &temp_dir
            .path()
            .join(format!("patch{}.vmcode", patch_number));
        std::fs::write(file_path, patch_number.to_string().repeat(patch_number)).unwrap();
        shorebird_info!(
            "Adding patch {} with contents {} hash {} at {}",
            patch_number,
            patch_number.to_string().repeat(patch_number),
            hash,
            file_path.display()
        );
        self.add_patch(patch_number, file_path, hash, signature)
    }
}

#[cfg(test)]
mod debug_tests {
    use tempdir::TempDir;

    use super::PatchManager;
    use crate::yaml::PatchVerificationMode;

    #[test]
    fn manage_patches_is_debug() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let patch_manager: Box<dyn super::ManagePatches> =
            Box::new(PatchManager::manager_for_test(&temp_dir));
        assert_eq!(format!("{:?}", patch_manager), "ManagePatches");
    }

    #[test]
    fn patch_manager_is_debug() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let patch_manager = PatchManager::new(
            temp_dir.path().to_owned(),
            Some("public_key"),
            PatchVerificationMode::default(),
        );
        let actual = format!("{:?}", patch_manager);
        assert!(actual.contains(r#"patches_state: PatchesState { last_booted_patch: None, next_boot_patch: None, currently_booting_patch: None, known_bad_patches: {} }, patch_public_key: Some("public_key")"#));
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
            .add_patch(
                1,
                Path::new("/path/to/file/that/does/not/exist"),
                "hash",
                None,
            )
            .is_err());
    }

    #[test]
    fn adds_patch_successfully() {
        let patch_number = 1;
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents).unwrap();

        assert!(manager
            .add_patch(
                patch_number,
                Path::new(file_path),
                "hash",
                Some("my_signature")
            )
            .is_ok());

        assert_eq!(
            manager.patches_state.next_boot_patch,
            Some(PatchMetadata {
                number: patch_number,
                size: patch_file_contents.len() as u64,
                hash: "hash".to_string(),
                signature: Some("my_signature".to_owned())
            })
        );
        assert!(!file_path.exists());
    }

    // InstallOnly mode signature verification tests - these verify that signature
    // checking happens at install time when using PatchVerificationMode::InstallOnly.

    // The constant values below were generated by taking an arbitrary sha256 hash (INFLATED_PATCH_HASH)
    // and using openssl to sign it with the private key corresponding to `PUBLIC_KEY`.

    // The base64-encoded public key in a DER format. This is required by ring to verify signatures.
    // See https://docs.rs/ring/latest/ring/signature/index.html#signing-and-verifying-with-rsa-pkcs1-15-padding
    const PUBLIC_KEY: &str = "MIIBCgKCAQEA2wdpEGbuvlPsb9i0qYrfMefJnEw1BHTi8SYZTKrXOvJWmEpPE1hWfbkvYzXu5a96gV1yocF3DMwn04VmRlKhC4AhsD0NL0UNhYhotbKG91Kwi1vAXpHhCdz5gQEBw0K1uB4Jz+zK6WK+31PryYpwLwbyXNqXoY8IAAUQ4STsHYV5w+BMSi8pepWMRd7DR9RHcbNOZlJvdBQ5NxvB4JN4dRMq8cC73ez1P9d7Dfwv3TWY+he9EmuXLT2UivZSlHIrGBa7MFfqyUe2ro0F7Te/B0si12itBbWIqycvqcXjeOPNn6WEpqN7IWjb9LUh162JyYaz5Lb/VeeJX8LKtElccwIDAQAB";

    // The message that was signed. In practice, this will be the sha256 hash of an inflated patch artifact.
    const INFLATED_PATCH_HASH: &str =
        "6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b";

    // The base64-encoded signature of `INFLATED_PATCH_HASH` created using the private key corresponding
    // to `PUBLIC_KEY`.
    const SIGNATURE: &str = "ZGccldv01XqHQ76bXuKV/9EQnNK0Q+reQ9bJHVnGfLldF+BLRx0divgPfKP5Df9BJPA3dw1Z1VortfepmMGebP3kS593l5zoktu9MIepxvRAFWNKE5PDTIIvCL/ddTPEHt6NNCeD6HLOMLzbEX3cFZa+lq3UymGi0aqA5DlXirJBGtopojc9nOXZ22n/qHNZIHEkGcqKbSMSK9oC55whKHnlJTbCXdmSyDc65B4PcgseqJom1riVK3XGW1YMrSpuMAU+CDT7HhdESmI1UtH1bYeBITfRhQztdDTfti2vJTf2Y+lYC99CFiISgD7f1m0KUcC+VnEAMZSYtgxSk6AX2A==";

    #[test]
    fn install_only_errs_if_public_key_is_invalid() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some("not a valid key"),
            PatchVerificationMode::InstallOnly,
        );

        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, "patch contents").unwrap();

        // In InstallOnly mode, fails at install time because the public key is invalid
        let result = manager.add_patch(1, file_path, INFLATED_PATCH_HASH, Some(SIGNATURE));
        assert!(result.is_err());
        assert!(manager.next_boot_patch().is_none());
    }

    #[test]
    fn install_only_errs_if_signature_is_missing_when_public_key_configured() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some(PUBLIC_KEY),
            PatchVerificationMode::InstallOnly,
        );

        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, "patch contents").unwrap();

        // In InstallOnly mode, fails at install time because signature is missing
        let result = manager.add_patch(1, file_path, INFLATED_PATCH_HASH, None);
        assert!(result.is_err());
        assert!(manager.next_boot_patch().is_none());
    }

    #[test]
    fn install_only_errs_if_signature_is_invalid() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some(PUBLIC_KEY),
            PatchVerificationMode::InstallOnly,
        );

        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, "patch contents").unwrap();

        // Using INFLATED_PATCH_HASH as a signature because it is valid base64, but not a valid signature.
        // In InstallOnly mode, this fails immediately at install time.
        let result =
            manager.add_patch(1, file_path, INFLATED_PATCH_HASH, Some(INFLATED_PATCH_HASH));
        assert!(result.is_err());
        assert!(manager.next_boot_patch().is_none());
    }

    #[test]
    fn install_only_succeeds_with_valid_signature() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some(PUBLIC_KEY),
            PatchVerificationMode::InstallOnly,
        );

        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, "patch contents").unwrap();

        // In InstallOnly mode, signature is verified at install time
        let result = manager.add_patch(1, file_path, INFLATED_PATCH_HASH, Some(SIGNATURE));
        assert!(result.is_ok());
        assert!(manager.next_boot_patch().is_some());
    }

    #[test]
    fn install_only_succeeds_with_any_signature_if_no_public_key() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            None, // No public key configured
            PatchVerificationMode::InstallOnly,
        );

        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, "patch contents").unwrap();

        // Without a public key, signature verification is skipped even in InstallOnly mode
        let result = manager.add_patch(1, file_path, "hash", Some("not a valid signature"));
        assert!(result.is_ok());
        assert!(manager.next_boot_patch().is_some());
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
        manager.patches_state.last_booted_patch = manager.patches_state.next_boot_patch.clone();
        assert_eq!(manager.last_successfully_booted_patch(), Some(expected));

        Ok(())
    }
}

#[cfg(test)]
mod next_boot_patch_tests {
    use super::*;
    use tempdir::TempDir;

    #[test]
    fn returns_none_if_no_next_boot_patch() {
        let temp_dir = TempDir::new("patch_manager").unwrap();
        let manager = PatchManager::manager_for_test(&temp_dir);
        assert!(manager.next_boot_patch().is_none());
    }

    #[test]
    fn returns_none_patch_if_first_patch_failed_to_boot() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Add a first patch and pretend it failed to boot.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_failure_for_patch(1)?;

        // Because there is no previous patch, we should not attempt to boot any patch.
        assert!(manager.next_boot_patch().is_none());
        assert!(manager.is_known_bad_patch(1));

        Ok(())
    }

    #[test]
    fn falls_back_to_last_booted_patch_if_still_bootable() -> Result<()> {
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;

        // Add patch 1, pretend it booted successfully.
        assert!(manager.add_patch(1, file_path, "hash", None).is_ok());
        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_success().is_ok());
        assert!(!manager.is_known_bad_patch(1));

        // Add patch 2, pretend it failed to boot.
        let file_path = &temp_dir.path().join("patch2.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(2, file_path, "hash", None).is_ok());
        assert!(manager.record_boot_start_for_patch(2).is_ok());
        assert!(manager.record_boot_failure_for_patch(2).is_ok());
        assert!(manager.is_known_bad_patch(2));

        // Verify that we will next attempt to boot from patch 1.
        assert_eq!(manager.next_boot_patch().unwrap().number, 1);

        Ok(())
    }

    #[test]
    fn adding_patch_deletes_unbooted_patch_not_last_booted() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Add patch 1 and boot it successfully.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_success()?;

        // Add patch 2 (not booted yet).
        manager.add_patch_for_test(&temp_dir, 2)?;

        let patch_1_artifact = manager.patch_artifact_path(1);
        let patch_2_artifact = manager.patch_artifact_path(2);
        assert!(patch_1_artifact.exists());
        assert!(patch_2_artifact.exists());

        // Add patch 3 — should delete patch 2 (unbooted), NOT patch 1 (last booted).
        manager.add_patch_for_test(&temp_dir, 3)?;

        assert!(
            patch_1_artifact.exists(),
            "Last booted patch 1 artifacts should NOT be deleted"
        );
        assert!(
            !patch_2_artifact.exists(),
            "Unbooted patch 2 artifacts should be deleted"
        );

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
        assert!(!manager.is_known_bad_patch(1));
        assert!(manager.is_known_bad_patch(2));

        Ok(())
    }
}

#[cfg(test)]
mod validate_next_boot_patch_tests {
    use super::*;
    use anyhow::Result;
    use tempdir::TempDir;

    #[test]
    fn does_nothing_if_no_next_boot_patch() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        assert!(manager.validate_next_boot_patch().is_ok());
        Ok(())
    }

    #[test]
    fn clears_next_boot_patch_if_it_is_not_bootable() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        manager.add_patch_for_test(&temp_dir, 1)?;

        // Write junk to the artifact, this should render the patch unbootable in the eyes
        // of the PatchManager.
        let artifact_path = manager.patch_artifact_path(1);
        std::fs::write(&artifact_path, "junk")?;

        assert!(manager.next_boot_patch().is_some());
        assert!(manager.validate_next_boot_patch().is_err());
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
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(1, file_path, "hash", None).is_ok());

        // Write junk to the artifact, this should render the patch unbootable in the eyes
        // of the PatchManager.
        let artifact_path = manager.patch_artifact_path(1);
        std::fs::write(&artifact_path, "junk")?;

        assert!(manager.next_boot_patch().is_some());
        assert!(manager.validate_next_boot_patch().is_err());
        assert!(manager.next_boot_patch().is_none());

        // Ensure the internal state is cleared.
        assert!(manager.patches_state.next_boot_patch.is_none());
        assert!(manager.patches_state.last_booted_patch.is_none());

        // The artifact should have been deleted.
        assert!(!&artifact_path.exists());

        Ok(())
    }

    #[test]
    fn does_not_fall_back_to_last_booted_patch_if_corrupted() -> Result<()> {
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;

        // Add patch 1, pretend it booted successfully.
        assert!(manager.add_patch(1, file_path, "hash", None).is_ok());
        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_success().is_ok());

        // Add patch 2, pretend it failed to boot.
        let file_path = &temp_dir.path().join("patch2.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager.add_patch(2, file_path, "hash", None).is_ok());
        assert!(manager.record_boot_start_for_patch(2).is_ok());
        assert!(manager.record_boot_failure_for_patch(2).is_ok());

        // Write junk to patch 1's artifact. This should prevent us from falling back to it.
        let patch_1_artifact_path = manager.patch_artifact_path(1);
        std::fs::write(patch_1_artifact_path, "junk")?;

        assert!(manager.next_boot_patch().is_some());
        assert!(manager.validate_next_boot_patch().is_err());

        // Verify that we will not attempt to boot from either patch.
        assert!(manager.next_boot_patch().is_none());

        // Patch 1 should *not* be considered bad, as we successfully booted from it and it only
        // became corrupted after that. Downloading it a second time might resolve the issue.
        assert!(!manager.is_known_bad_patch(1));

        // Patch 2 failed to boot, so it should be considered bad.
        assert!(manager.is_known_bad_patch(2));

        Ok(())
    }

    // The constant values below were generated by taking an arbitrary sha256 hash (INFLATED_PATCH_HASH)
    // and using openssl to sign it with the private key corresponding to `PUBLIC_KEY`.

    // The base64-encoded public key in a DER format. This is required by ring to verify signatures.
    // See https://docs.rs/ring/latest/ring/signature/index.html#signing-and-verifying-with-rsa-pkcs1-15-padding
    const PUBLIC_KEY: &str = "MIIBCgKCAQEA2wdpEGbuvlPsb9i0qYrfMefJnEw1BHTi8SYZTKrXOvJWmEpPE1hWfbkvYzXu5a96gV1yocF3DMwn04VmRlKhC4AhsD0NL0UNhYhotbKG91Kwi1vAXpHhCdz5gQEBw0K1uB4Jz+zK6WK+31PryYpwLwbyXNqXoY8IAAUQ4STsHYV5w+BMSi8pepWMRd7DR9RHcbNOZlJvdBQ5NxvB4JN4dRMq8cC73ez1P9d7Dfwv3TWY+he9EmuXLT2UivZSlHIrGBa7MFfqyUe2ro0F7Te/B0si12itBbWIqycvqcXjeOPNn6WEpqN7IWjb9LUh162JyYaz5Lb/VeeJX8LKtElccwIDAQAB";

    // The message that was signed. In practice, this will be the sha256 hash of an inflated patch artifact.
    const INFLATED_PATCH_HASH: &str =
        "6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b";

    // The base64-encoded signature of `INFLATED_PATCH_HASH` created using the private key corresponding
    // to `PUBLIC_KEY`.
    const SIGNATURE: &str = "ZGccldv01XqHQ76bXuKV/9EQnNK0Q+reQ9bJHVnGfLldF+BLRx0divgPfKP5Df9BJPA3dw1Z1VortfepmMGebP3kS593l5zoktu9MIepxvRAFWNKE5PDTIIvCL/ddTPEHt6NNCeD6HLOMLzbEX3cFZa+lq3UymGi0aqA5DlXirJBGtopojc9nOXZ22n/qHNZIHEkGcqKbSMSK9oC55whKHnlJTbCXdmSyDc65B4PcgseqJom1riVK3XGW1YMrSpuMAU+CDT7HhdESmI1UtH1bYeBITfRhQztdDTfti2vJTf2Y+lYC99CFiISgD7f1m0KUcC+VnEAMZSYtgxSk6AX2A==";

    // Strict mode boot-time signature verification tests.
    // In Strict mode, signature verification happens at boot time (validate_next_boot_patch),
    // not at install time. This provides protection against post-install tampering.

    #[test]
    fn strict_mode_succeeds_with_valid_signature_at_boot_time() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some(PUBLIC_KEY),
            PatchVerificationMode::Strict,
        );

        // In Strict mode, add_patch does NOT verify signature (that happens at boot time)
        manager.add_signed_patch_for_test(&temp_dir, 1, INFLATED_PATCH_HASH, Some(SIGNATURE))?;

        // Boot-time validation verifies the signature by computing hash and checking signature
        assert!(manager.next_boot_patch().is_some());
        assert!(manager.validate_next_boot_patch().is_ok());
        assert!(manager.next_boot_patch().is_some());

        let patch = manager.next_boot_patch().unwrap();
        assert_eq!(patch.number, 1);

        Ok(())
    }

    #[test]
    fn succeeds_with_arbitrary_signature_if_no_public_key() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        // Create a PatchManager without a public key - signature verification is skipped.
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        manager.add_signed_patch_for_test(
            &temp_dir,
            1,
            INFLATED_PATCH_HASH,
            Some("not a valid signature"),
        )?;

        // Without a public key, boot-time validation only checks file existence and size
        assert!(manager.next_boot_patch().is_some());
        assert!(manager.validate_next_boot_patch().is_ok());
        assert!(manager.next_boot_patch().is_some());
        let patch = manager.next_boot_patch().unwrap();
        assert_eq!(patch.number, 1);

        Ok(())
    }

    #[test]
    fn strict_mode_fails_boot_validation_if_signature_missing() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some(PUBLIC_KEY),
            PatchVerificationMode::Strict,
        );

        // In Strict mode, add_patch succeeds without signature (no install-time check)
        manager.add_signed_patch_for_test(&temp_dir, 1, INFLATED_PATCH_HASH, None)?;

        assert!(manager.next_boot_patch().is_some());
        // But boot-time validation fails because signature is required
        assert!(manager.validate_next_boot_patch().is_err());
        assert!(manager.next_boot_patch().is_none());

        Ok(())
    }

    #[test]
    fn strict_mode_fails_boot_validation_if_signature_invalid() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some(PUBLIC_KEY),
            PatchVerificationMode::Strict,
        );

        // In Strict mode, add_patch succeeds with invalid signature (no install-time check)
        manager.add_signed_patch_for_test(
            &temp_dir,
            1,
            INFLATED_PATCH_HASH,
            Some(INFLATED_PATCH_HASH), // Using hash as signature, which is invalid
        )?;

        assert!(manager.next_boot_patch().is_some());
        // But boot-time validation fails because signature doesn't verify
        assert!(manager.validate_next_boot_patch().is_err());
        assert!(manager.next_boot_patch().is_none());

        Ok(())
    }

    #[test]
    fn strict_mode_fails_boot_validation_if_public_key_invalid() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some("not a valid key"),
            PatchVerificationMode::Strict,
        );

        // In Strict mode, add_patch succeeds (no install-time check)
        manager.add_signed_patch_for_test(&temp_dir, 1, INFLATED_PATCH_HASH, Some(SIGNATURE))?;

        assert!(manager.next_boot_patch().is_some());
        // But boot-time validation fails because public key can't be used
        assert!(manager.validate_next_boot_patch().is_err());
        assert!(manager.next_boot_patch().is_none());

        Ok(())
    }

    #[test]
    fn strict_mode_detects_tampered_patch_at_boot_time() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::new(
            temp_dir.path().to_path_buf(),
            Some(PUBLIC_KEY),
            PatchVerificationMode::Strict,
        );

        // Install a valid patch
        manager.add_signed_patch_for_test(&temp_dir, 1, INFLATED_PATCH_HASH, Some(SIGNATURE))?;

        // Tamper with the patch file after installation
        let patch_path = manager.patch_artifact_path(1);
        std::fs::write(&patch_path, "tampered content")?;

        assert!(manager.next_boot_patch().is_some());
        // Boot-time validation detects tampering: computed hash doesn't match signature
        assert!(manager.validate_next_boot_patch().is_err());
        assert!(manager.next_boot_patch().is_none());

        Ok(())
    }
}

#[cfg(test)]
mod fall_back_tests {
    use super::*;

    #[test]
    fn does_nothing_if_no_patch_exists() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        assert!(manager.patches_state.last_booted_patch.is_none());
        assert!(manager.patches_state.next_boot_patch.is_none());

        manager.try_fall_back_from_patch(1)?;

        assert!(manager.patches_state.last_booted_patch.is_none());
        assert!(manager.patches_state.next_boot_patch.is_none());

        Ok(())
    }

    #[test]
    fn sets_next_patch_to_latest_patch_if_both_are_present() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Download and successfully boot from patch 1
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_success()?;

        // Download and fall back from patch 2
        manager.add_patch_for_test(&temp_dir, 2)?;

        manager.try_fall_back_from_patch(2)?;

        assert_eq!(manager.patches_state.last_booted_patch.unwrap().number, 1);
        assert_eq!(manager.patches_state.next_boot_patch.unwrap().number, 1);

        Ok(())
    }

    #[test]
    fn clears_next_and_last_patches_if_both_fail_validation() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Download and successfully boot from patch 1, and then corrupt it on disk.
        manager.add_patch_for_test(&temp_dir, 1)?;
        manager.record_boot_start_for_patch(1)?;
        manager.record_boot_success()?;
        let patch_1_path = manager.patch_artifact_path(1);
        std::fs::write(patch_1_path, "junk junk junk")?;

        // Download and fall back from patch 2
        manager.add_patch_for_test(&temp_dir, 2)?;

        manager.try_fall_back_from_patch(2)?;

        // Neither patch should exist.
        assert!(manager.patches_state.last_booted_patch.is_none());
        assert!(manager.patches_state.next_boot_patch.is_none());

        Ok(())
    }

    #[test]
    fn does_not_clear_next_patch_if_changed_since_boot_start() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // Simulate a situation where we download both patches 1 and 2.
        manager.add_patch_for_test(&temp_dir, 1)?;

        // Start booting from patch 1.
        manager.record_boot_start_for_patch(1)?;

        // Download patch 2 before patch 1 finishes booting.
        manager.add_patch_for_test(&temp_dir, 2)?;

        manager.record_boot_failure_for_patch(1)?;

        manager.try_fall_back_from_patch(1)?;

        assert!(manager.patches_state.last_booted_patch.is_none());
        assert_eq!(
            manager
                .patches_state
                .next_boot_patch
                .clone()
                .unwrap()
                .number,
            2
        );
        assert!(manager.is_known_bad_patch(1));

        Ok(())
    }

    #[test]
    fn succeeds_if_deleting_artifacts_fails() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

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

        manager.try_fall_back_from_patch(2)?;

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
        let mut manager = PatchManager::manager_for_test(&temp_dir);

        // This should fail because no patches have been added.
        assert!(manager.record_boot_success().is_err());

        Ok(())
    }

    #[test]
    fn errs_if_patch_number_does_not_match_next_patch() -> Result<()> {
        let patch_number = 1;
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager
            .add_patch(patch_number, file_path, "hash", None)
            .is_ok());
        assert!(manager.record_boot_success().is_err());

        Ok(())
    }

    #[test]
    fn succeeds_when_provided_next_boot_patch_number() -> Result<()> {
        let patch_number = 1;
        let patch_file_contents = "patch contents";
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);
        let file_path = &temp_dir.path().join("patch1.vmcode");
        std::fs::write(file_path, patch_file_contents)?;
        assert!(manager
            .add_patch(patch_number, file_path, "hash", None)
            .is_ok());

        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_success().is_ok());

        Ok(())
    }

    #[test]
    fn deletes_other_patch_artifacts() -> Result<()> {
        let temp_dir = TempDir::new("patch_manager")?;
        let mut manager = PatchManager::manager_for_test(&temp_dir);

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
        let mut manager = PatchManager::manager_for_test(&temp_dir);

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
        assert!(!manager.is_known_bad_patch(1));
        let succeeded_patch_artifact_path = manager.patch_artifact_path(1);

        manager.add_patch_for_test(&temp_dir, 2)?;
        let failed_patch_artifact_path = manager.patch_artifact_path(2);

        // Make sure patch artifacts exist
        assert!(failed_patch_artifact_path.exists());
        assert!(succeeded_patch_artifact_path.exists());

        assert!(manager.record_boot_start_for_patch(2).is_ok());
        assert!(manager.record_boot_failure_for_patch(2).is_ok());
        assert!(!failed_patch_artifact_path.exists());
        assert!(manager.is_known_bad_patch(2));

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
        assert!(!manager.is_known_bad_patch(1));

        // Now pretend it failed to boot
        assert!(manager.record_boot_start_for_patch(1).is_ok());
        assert!(manager.record_boot_failure_for_patch(1).is_ok());
        assert!(manager.last_successfully_booted_patch().is_none());
        assert!(manager.next_boot_patch().is_none());
        assert!(manager.is_known_bad_patch(1));
        assert!(!patch_artifact_path.exists());

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
