// This file's job is to be the Rust API for the updater.

use std::fmt::{Display, Formatter};
use std::fs;
#[cfg(any(target_os = "android", test))]
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use anyhow::bail;
use anyhow::Context;

use crate::cache::{PatchInfo, UpdaterState};
use crate::config::{set_config, with_config, UpdateConfig};
use crate::logging::init_logging;
use crate::network::{
    download_to_path, report_successful_patch_install, send_patch_check_request, NetworkHooks,
    PatchCheckResponse,
};
use crate::updater_lock::{with_updater_thread_lock, UpdaterLockState};
use crate::yaml::YamlConfig;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as error, println as debug}; // Workaround to use println! for logs.

#[cfg(test)]
// Expose testing_reset_config for integration tests.
pub use crate::config::testing_reset_config;
#[cfg(test)]
pub use crate::network::{
    testing_set_network_hooks, DownloadFileFn, Patch, PatchCheckRequest, PatchCheckRequestFn,
};

pub enum UpdateStatus {
    NoUpdate,
    UpdateInstalled,
    UpdateHadError,
}

impl Display for UpdateStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateStatus::NoUpdate => write!(f, "No update"),
            UpdateStatus::UpdateInstalled => write!(f, "Update installed"),
            UpdateStatus::UpdateHadError => write!(f, "Update had error"),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum UpdateError {
    InvalidArgument(String, String),
    InvalidState(String),
    BadServerResponse,
    FailedToSaveState,
    ConfigNotInitialized,
    UpdateAlreadyInProgress,
}

impl std::error::Error for UpdateError {}

impl Display for UpdateError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            UpdateError::InvalidArgument(name, value) => {
                write!(f, "Invalid Argument: {} -> {}", name, value)
            }
            UpdateError::InvalidState(msg) => write!(f, "Invalid State: {}", msg),
            UpdateError::FailedToSaveState => write!(f, "Failed to save state"),
            UpdateError::BadServerResponse => write!(f, "Bad server response"),
            UpdateError::ConfigNotInitialized => write!(f, "Config not initialized"),
            UpdateError::UpdateAlreadyInProgress => {
                write!(f, "Update already in progress")
            }
        }
    }
}

// AppConfig is the rust API.  ResolvedConfig is the internal storage.
// However rusty api would probably used &str instead of String,
// but making &str from CStr* is a bit of a pain.
pub struct AppConfig {
    pub cache_dir: String,
    pub release_version: String,
    pub original_libapp_paths: Vec<String>,
}

// On Android we don't use a direct path to libapp.so, but rather a data dir
// and a hard-coded name for the libapp file which we look up in the
// split APKs in that datadir. On other platforms we just use a path.
#[cfg(not(any(target_os = "android", test)))]
fn libapp_path_from_settings(original_libapp_paths: &Vec<String>) -> Result<PathBuf, UpdateError> {
    let first = original_libapp_paths
        .first()
        .ok_or(UpdateError::InvalidArgument(
            "original_libapp_paths".to_string(),
            "empty".to_string(),
        ));
    first.map(|path| PathBuf::from(path))
}

/// Initialize the updater library.
/// Takes a AppConfig struct and a yaml string.
/// The yaml string is the contents of the shorebird.yaml file.
/// The AppConfig struct is information about the running app and where
/// the updater should keep its cache.
pub fn init(app_config: AppConfig, yaml: &str) -> Result<(), UpdateError> {
    #[cfg(any(target_os = "android", test))]
    use crate::android::libapp_path_from_settings;

    init_logging();
    let config = YamlConfig::from_yaml(&yaml)
        .map_err(|err| UpdateError::InvalidArgument("yaml".to_string(), err.to_string()))?;

    let libapp_path = libapp_path_from_settings(&app_config.original_libapp_paths)?;
    debug!("libapp_path: {:?}", libapp_path);
    set_config(app_config, libapp_path, config, NetworkHooks::default())
        .map_err(|err| UpdateError::InvalidState(err.to_string()))
}

pub fn should_auto_update() -> anyhow::Result<bool> {
    with_config(|config| Ok(config.auto_update))
}

fn check_for_update_internal() -> anyhow::Result<PatchCheckResponse> {
    with_config(|config| {
        // Load UpdaterState from disk
        // If there is no state, make an empty state.
        let state = UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
        send_patch_check_request(&config, &state)
    })
}

/// Synchronously checks for an update and returns true if an update is available.
pub fn check_for_update() -> anyhow::Result<bool> {
    check_for_update_internal().map(|res| res.patch_available)
}

fn check_hash(path: &Path, expected_string: &str) -> anyhow::Result<()> {
    let expected = hex::decode(expected_string).context("Invalid hash string from server.")?;

    use sha2::{Digest, Sha256}; // Digest is needed for Sha256::new();

    // Based on guidance from:
    // https://github.com/RustCrypto/hashes#hashing-readable-objects

    let mut file = fs::File::open(&path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    // Check that the length from copy is the same as the file size?
    let hash = hasher.finalize();
    let hash_matches = hash.as_slice() == expected;
    // This is a common error for developers.  We could avoid it entirely
    // by sending the hash of libapp.so to the server and having the
    // server only send updates when the hash matches.
    // https://github.com/shorebirdtech/updater/issues/56
    if !hash_matches {
        bail!(
            "Update rejected: hash mismatch. Update was downloaded but \
            contents did not match the expected hash. This is most often \
            caused by using the same version number with a different app \
            binary. Path: {:?}, expected: {}, got: {}",
            path,
            expected_string,
            hex::encode(hash)
        );
    } else {
        debug!("Hash match: {:?}", path);
    }
    return Ok(());
}

// This is just a place to put our terrible android hacks.
// And also avoid (for now) dealing with inflating patches on iOS.
#[cfg(any(target_os = "android", test))]
fn prepare_for_install(
    config: &UpdateConfig,
    download_path: &Path,
    output_path: &Path,
) -> anyhow::Result<()> {
    // We abuse libapp_path to actually be the path to the data dir for now.
    // This is an abuse because the variable name is libapp_path, but
    // we're making it point to a the app_data directory instead.
    let app_dir = &config.libapp_path;
    debug!("app_dir: {:?}", app_dir);
    let base_r = crate::android::open_base_lib(&app_dir, "libapp.so")?;
    inflate(&download_path, base_r, &output_path)
}

#[cfg(not(any(target_os = "android", test)))]
fn prepare_for_install(
    _config: &UpdateConfig,
    download_path: &Path,
    output_path: &Path,
) -> anyhow::Result<()> {
    // On iOS we don't yet support compressed patches, just copy the file.
    fs::copy(download_path, output_path)?;
    Ok(())
}

fn copy_update_config() -> anyhow::Result<UpdateConfig> {
    with_config(|config: &UpdateConfig| Ok(config.clone()))
}

// Callers must possess the Updater lock, but we don't care about the contents
// since they're empty.
fn update_internal(_: &UpdaterLockState) -> anyhow::Result<UpdateStatus> {
    // Only one copy of Update can be running at a time.
    // Update will take the global Updater lock.
    // Update will need to take the Config lock at times, but will only
    // do so as long as is needed to read from the config and will not
    // hold the config lock across network requests.
    // Steps:
    // Checks Update lock, if held, return error, otherwise takes lock for
    // entire duration of update.
    // Loads state from disk (holds Config lock while reading).
    // Uses current update information to build request and send to server.
    // If update is not available, returns.
    // Update is available, so uses returned information to download update.
    // Downloads update to a temporary location.
    // Checks hash of downloaded file.
    // Takes Config lock and installs patch.
    // Saves state to disk (holds Config lock while writing).

    let config = copy_update_config()?;

    // Load the state from disk.
    let mut state = UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
    // Check for update.
    let response = send_patch_check_request(&config, &state)?;
    if !response.patch_available {
        return Ok(UpdateStatus::NoUpdate);
    }

    let patch = response.patch.ok_or(UpdateError::BadServerResponse)?;

    let download_dir = PathBuf::from(&config.download_dir);
    let download_path = download_dir.join(patch.number.to_string());
    // Consider supporting allowing the system to download for us (e.g. iOS).
    download_to_path(&config.network_hooks, &patch.download_url, &download_path)?;

    let output_path = download_dir.join(format!("{}.full", patch.number.to_string()));
    // Should not pass config, rather should read necessary information earlier.
    prepare_for_install(&config, &download_path, &output_path)?;

    // Check the hash before moving into place.
    check_hash(&output_path, &patch.hash).context(format!(
        "This app reports version {}, but the binary is different from \
        the version {} that was submitted to Shorebird.",
        config.release_version, config.release_version
    ))?;

    // We're abusing the config lock as a UpdateState lock for now.
    // This makes it so we never try to write to the UpdateState file from
    // two threads at once. We could give UpdateState its own lock instead.
    with_config(|_| {
        let patch_info = PatchInfo {
            path: output_path,
            number: patch.number,
        };
        // Move/state update should be "atomic" (it isn't today).
        state.install_patch(patch_info)?;
        info!("Patch {} successfully installed.", patch.number);
        // Should set some state to say the status is "update required" and that
        // we now have a different "next" version of the app from the current
        // booted version (patched or not).
        return Ok(UpdateStatus::UpdateInstalled);
    })
}

/// Synchronously checks for an update and downloads and installs it if available.
pub fn update() -> anyhow::Result<UpdateStatus> {
    with_updater_thread_lock(update_internal)
}

/// Given a path to a patch file, and a base file, apply the patch to the base
/// and write the result to the output path.
#[cfg(any(target_os = "android", test))]
fn inflate<RS>(patch_path: &Path, base_r: RS, output_path: &Path) -> anyhow::Result<()>
where
    RS: Read + Seek,
{
    use comde::de::Decompressor;
    use comde::zstd::ZstdDecompressor;
    debug!("Patch is compressed, inflating...");
    use std::io::{BufReader, BufWriter};

    // Open all our files first for error clarity.  Otherwise we might see
    // PipeReader/Writer errors instead of file open errors.
    debug!("Reading patch file: {:?}", patch_path);
    let compressed_patch_r = BufReader::new(
        fs::File::open(patch_path)
            .context(format!("Failed to open patch file: {:?}", patch_path))?,
    );
    let output_file_w = fs::File::create(&output_path)?;

    // Set up a pipe to connect the writing from the decompression thread
    // to the reading of the decompressed patch data on this thread.
    let (patch_r, patch_w) = pipe::pipe();

    let decompress = ZstdDecompressor::new();
    // Spawn a thread to run the decompression in parallel to the patching.
    // decompress.copy will block on the pipe being full (I think) and then
    // when it returns the thread will exit.
    std::thread::spawn(move || {
        // If this thread fails, undoubtedly the main thread will fail too.
        // Most important is to not crash.
        let result = decompress.copy(compressed_patch_r, patch_w);
        if let Err(err) = result {
            error!("Decompression thread failed: {err}");
        }
    });

    // Do the patch, using the uncompressed patch data from the pipe.
    let mut fresh_r = bipatch::Reader::new(patch_r, base_r)?;

    // Write out the resulting patched file to the new location.
    let mut output_w = BufWriter::new(output_file_w);
    std::io::copy(&mut fresh_r, &mut output_w)?;
    Ok(())
}

/// The patch which will be run on next boot (which may still be the same
/// as the current boot).
/// This may be changed any time update() or start_update_thread() are called.
pub fn next_boot_patch() -> anyhow::Result<Option<PatchInfo>> {
    with_config(|config| {
        let state = UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
        return Ok(state.next_boot_patch());
    })
}

/// The patch which is currently booted.  This is None until
/// report_launch_start() is called at which point it is copied from
/// next_boot_patch.
pub fn current_boot_patch() -> anyhow::Result<Option<PatchInfo>> {
    with_config(|config| {
        let state = UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
        return Ok(state.current_boot_patch());
    })
}

pub fn report_launch_start() -> anyhow::Result<()> {
    with_config(|config| {
        let mut state =
            UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
        // Validate that we have an installed patch.
        // Make that patch the "booted" patch.
        state.activate_current_patch()?;
        state.save()
    })
}

/// Report that the current active path failed to launch.
/// This will mark the patch as bad and activate the next best patch.
pub fn report_launch_failure() -> anyhow::Result<()> {
    info!("Reporting failed launch.");
    with_config(|config| {
        let mut state =
            UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);

        let patch =
            state
                .current_boot_patch()
                .ok_or(anyhow::Error::from(UpdateError::InvalidState(
                    "No current patch".to_string(),
                )))?;
        // Ignore the error here, we'll try to activate the next best patch
        // even if we fail to mark this one as bad (because it was already bad).
        let _ = state.mark_patch_as_bad(patch.number);
        state
            .activate_latest_bootable_patch()
            .map_err(|err| anyhow::Error::from(err))
    })
}

pub fn report_launch_success() -> anyhow::Result<()> {
    with_config(|config| {
        let mut state =
            UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);

        if let Some(patch) = state.current_boot_patch() {
            if !state.is_known_good_patch(patch.number) {
                // Ignore the error here, we'll try to activate the next best patch
                // even if we fail to mark this one as good.
                if state.mark_patch_as_good(patch.number).is_ok() {
                    let report_result =
                        report_successful_patch_install(&config, &state, patch.number);
                    if let Err(err) = report_result {
                        error!("Failed to report successful patch install: {:?}", err);
                    }
                }
            }

            state
                .save()
                .map_err(|_| anyhow::Error::from(UpdateError::FailedToSaveState))
        } else {
            Ok(())
        }
    })
}

/// This does not return status.  The only output is the change to the saved
/// cache. The Engine calls this during boot and it will check for an update
/// and install it if available.
pub fn start_update_thread() {
    std::thread::spawn(move || {
        let result = update();
        let status = match result {
            Ok(status) => status,
            Err(err) => {
                error!("Update failed: {:?}", err);
                UpdateStatus::UpdateHadError
            }
        };
        info!("Update thread finished with status: {}", status);
    });
}

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use std::fs;
    use tempdir::TempDir;

    use crate::config::testing_reset_config;

    fn init_for_testing(tmp_dir: &TempDir) {
        testing_reset_config();
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
        crate::init(
            crate::AppConfig {
                cache_dir: cache_dir.clone(),
                release_version: "1.0.0+1".to_string(),
                original_libapp_paths: vec!["/dir/lib/arch/libapp.so".to_string()],
            },
            "app_id: 1234",
        )
        .unwrap();
    }

    #[serial]
    #[test]
    fn ignore_version_after_marked_bad() {
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir);

        use crate::cache::{PatchInfo, UpdaterState};
        use crate::config::with_config;

        // Install a fake patch.
        with_config(|config| {
            let download_dir = std::path::PathBuf::from(&config.download_dir);
            let artifact_path = download_dir.join("1");
            fs::create_dir_all(&download_dir).unwrap();
            fs::write(&artifact_path, "hello").unwrap();

            let mut state =
                UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
            state
                .install_patch(PatchInfo {
                    path: artifact_path,
                    number: 1,
                })
                .expect("move failed");
            state.save().expect("save failed");
            Ok(())
        })
        .unwrap();
        assert!(crate::next_boot_patch().unwrap().is_some());
        // pretend we booted from it
        crate::report_launch_start().unwrap();
        crate::report_launch_success().unwrap();
        assert!(crate::next_boot_patch().unwrap().is_some());
        // mark it bad.
        crate::report_launch_failure().unwrap();
        // Technically might need to "reload"
        // ask for current patch (should get none).
        assert!(crate::next_boot_patch().unwrap().is_none());
    }

    #[test]
    fn hash_matches() {
        let tmp_dir = TempDir::new("example").unwrap();

        let input_path = tmp_dir.path().join("input");
        fs::write(&input_path, "hello world").unwrap();

        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(super::check_hash(&input_path, expected).is_ok());

        // modify hash to not match
        let expected = "a94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        // We don't check the full error string because it contains a path
        // which varies on each run.
        assert!(super::check_hash(&input_path, expected)
            .unwrap_err()
            .to_string()
            .contains("Update rejected: hash mismatch. Update was downloaded"));

        // invalid hashes should not match either
        let expected = "foo";
        assert_eq!(
            super::check_hash(&input_path, expected)
                .unwrap_err()
                .to_string(),
            "Invalid hash string from server."
        );

        // Server used to send "#" and we'd allow it, but now we don't.
        let expected = "#";
        assert_eq!(
            super::check_hash(&input_path, expected)
                .unwrap_err()
                .to_string(),
            "Invalid hash string from server."
        );
    }

    #[serial]
    #[test]
    fn init_missing_yaml() {
        let tmp_dir = TempDir::new("example").unwrap();
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
        assert_eq!(
            crate::init(
                crate::AppConfig {
                    cache_dir: cache_dir.clone(),
                    release_version: "1.0.0+1".to_string(),
                    original_libapp_paths: vec!["original_libapp_path".to_string()],
                },
                "",
            ),
            Err(crate::UpdateError::InvalidArgument(
                "yaml".to_string(),
                "missing field `app_id`".to_string()
            ))
        );
    }

    #[serial]
    #[test]
    fn report_launch_result_with_no_current_patch() {
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir);
        assert_eq!(
            crate::report_launch_failure()
                .unwrap_err()
                .downcast::<crate::UpdateError>()
                .unwrap(),
            crate::UpdateError::InvalidState("No current patch".to_string())
        );
        assert!(crate::report_launch_success().is_ok());
    }

    #[serial]
    #[test]
    fn report_launch_success_with_patch() {
        use crate::cache::{PatchInfo, UpdaterState};
        use crate::config::with_config;
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir);

        // Install a fake patch.
        with_config(|config| {
            let download_dir = std::path::PathBuf::from(&config.download_dir);
            let artifact_path = download_dir.join("1");
            fs::create_dir_all(&download_dir).unwrap();
            fs::write(&artifact_path, "hello").unwrap();

            let mut state =
                UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
            state
                .install_patch(PatchInfo {
                    path: artifact_path,
                    number: 1,
                })
                .expect("move failed");
            state.save().expect("save failed");
            Ok(())
        })
        .unwrap();

        // Pretend we booted from it.
        crate::report_launch_start().unwrap();

        let next_boot_patch = crate::next_boot_patch().unwrap().unwrap();
        with_config(|config| {
            let state =
                UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
            assert!(!state.is_known_good_patch(next_boot_patch.number));
            Ok(())
        })
        .unwrap();

        super::report_launch_success().unwrap();

        with_config(|config| {
            let state =
                UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
            assert!(state.is_known_good_patch(next_boot_patch.number));
            Ok(())
        })
        .unwrap();
    }
}
