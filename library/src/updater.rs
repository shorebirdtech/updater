// This file's job is to be the Rust API for the updater.

use std::fmt::{Debug, Display, Formatter};
use std::fs::{self};
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use dyn_clone::DynClone;

use crate::cache::{PatchInfo, UpdaterState};
use crate::config::{set_config, with_config, UpdateConfig};
use crate::events::{EventType, PatchEvent};
use crate::logging::init_logging;
use crate::network::{download_to_path, patches_check_url, NetworkHooks, PatchCheckRequest};
use crate::updater_lock::{with_updater_thread_lock, UpdaterLockState};
use crate::yaml::YamlConfig;

#[cfg(test)]
// Expose testing_reset_config for integration tests.
pub use crate::config::testing_reset_config;
#[cfg(test)]
pub use crate::network::{DownloadFileFn, Patch, PatchCheckRequestFn};

#[derive(Debug, PartialEq)]
pub enum UpdateStatus {
    NoUpdate,
    UpdateInstalled,
    UpdateHadError,
    UpdateIsBadPatch,
}

impl Display for UpdateStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateStatus::NoUpdate => write!(f, "No update"),
            UpdateStatus::UpdateInstalled => write!(f, "Update installed"),
            UpdateStatus::UpdateHadError => write!(f, "Update had error"),
            UpdateStatus::UpdateIsBadPatch => write!(
                f,
                "Update available but previously failed to install. Not installing."
            ),
        }
    }
}

/// Whether a patch is OK to install, and if not, why.
pub enum ShouldInstallPatchCheckResult {
    PatchOkToInstall,
    PatchKnownBad,
    PatchAlreadyInstalled,
}

/// Returned when a call to `init` is not successful. These indicate that the specific call to
/// `init` was not successful, but the library may still be in a valid state (e.g., if
/// `AlreadyInitialized` is returned, the library is still initialized). Callers can safely ignore
/// these errors if they are not interested in the specific reason why `init` failed.
#[derive(Debug, PartialEq)]
pub enum InitError {
    InvalidArgument(String, String),
    AlreadyInitialized,
    FailedToCleanUpFailedPatch,
}

impl std::error::Error for InitError {}

impl Display for InitError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            InitError::InvalidArgument(name, value) => {
                write!(f, "Invalid Argument: {name} -> {value}")
            }
            InitError::AlreadyInitialized => write!(f, "Shorebird has already been initialized."),
            InitError::FailedToCleanUpFailedPatch => {
                write!(f, "Failed to clean up after a failed patch.")
            }
        }
    }
}

/// Returned when a function that is part of the update lifecycle fails.
#[derive(Debug, PartialEq)]
pub enum UpdateError {
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
            UpdateError::InvalidState(msg) => write!(f, "Invalid State: {msg}"),
            UpdateError::FailedToSaveState => write!(f, "Failed to save state"),
            UpdateError::BadServerResponse => write!(f, "Bad server response"),
            UpdateError::ConfigNotInitialized => write!(f, "Config not initialized"),
            UpdateError::UpdateAlreadyInProgress => {
                write!(f, "Update already in progress")
            }
        }
    }
}

// `AppConfig` is the rust API.
// However rusty api would probably used `&str` instead of `String`,
// but making `&str` from `CStr*` is a bit of a pain.
pub struct AppConfig {
    pub app_storage_dir: String,
    pub code_cache_dir: String,
    pub release_version: String,
    pub original_libapp_paths: Vec<String>,
}

pub trait ReadSeek: Read + Seek {}

/// Provides an interface to get an opaque ReadSeek object for a given path.
/// This is used to provide a way to read the patch base file on iOS.
pub trait ExternalFileProvider: Debug + Send + DynClone {
    fn open(&self) -> anyhow::Result<Box<dyn ReadSeek>>;
}

// This is required for ExternalFileProvider to be used as a field in the Clone-able
// UpdateConfig struct.
dyn_clone::clone_trait_object!(ExternalFileProvider);

// On Android we don't use a direct path to libapp.so, but rather a data dir
// and a hard-coded name for the libapp file which we look up in the
// split APKs in that datadir. On other platforms we just use a path.
#[cfg(not(any(target_os = "android", test)))]
fn libapp_path_from_settings(original_libapp_paths: &[String]) -> Result<PathBuf, InitError> {
    let first = original_libapp_paths
        .first()
        .ok_or(InitError::InvalidArgument(
            "original_libapp_paths".to_string(),
            "empty".to_string(),
        ));
    first.map(PathBuf::from)
}

pub fn with_state<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&UpdaterState) -> anyhow::Result<R>,
{
    with_config(|config| {
        let state = UpdaterState::load_or_new_on_error(
            &config.storage_dir,
            &config.release_version,
            config.patch_public_key.as_deref(),
            config.patch_verification,
        );
        f(&state)
    })
}

pub fn with_mut_state<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&mut UpdaterState) -> anyhow::Result<R>,
{
    with_config(|config| {
        let mut state = UpdaterState::load_or_new_on_error(
            &config.storage_dir,
            &config.release_version,
            config.patch_public_key.as_deref(),
            config.patch_verification,
        );
        f(&mut state)
    })
}

/// Initialize the updater library.
/// Takes a `AppConfig` struct and a yaml string.
/// The yaml string is the contents of the `shorebird.yaml` file.
/// The `AppConfig` struct is information about the running app and where
/// the updater should keep its cache.
pub fn init(
    app_config: AppConfig,
    file_provider: Box<dyn ExternalFileProvider>,
    yaml: &str,
) -> Result<(), InitError> {
    #[cfg(any(target_os = "android", test))]
    use crate::android::libapp_path_from_settings;

    init_logging();
    let config = YamlConfig::from_yaml(yaml)
        .map_err(|err| InitError::InvalidArgument("yaml".to_string(), err.to_string()))?;

    let libapp_path = libapp_path_from_settings(&app_config.original_libapp_paths)?;
    shorebird_debug!("libapp_path: {:?}", libapp_path);
    let set_config_result = set_config(
        app_config,
        file_provider,
        libapp_path,
        &config,
        NetworkHooks::default(),
    );

    // set_config will return an error if the config is already initialized. This should not cause
    // init to fail.
    if set_config_result.is_err() {
        return Err(InitError::AlreadyInitialized);
    }

    handle_prior_boot_failure_if_necessary()
}

/// If, at initialization time, we detect that we were in the process of booting a patch, report a
/// failure to boot for that patch and queue an event to report the failure.
pub fn handle_prior_boot_failure_if_necessary() -> Result<(), InitError> {
    with_config(|config| {
        let mut state = UpdaterState::load_or_new_on_error(
            &config.storage_dir,
            &config.release_version,
            config.patch_public_key.as_deref(),
            config.patch_verification,
        );
        if let Some(patch) = state.currently_booting_patch() {
            state.record_boot_failure_for_patch(patch.number)?;
            state.queue_event(PatchEvent::new(
                config,
                EventType::PatchInstallFailure,
                patch.number,
                state.client_id(),
                Some(
                    format!(
                        "Patch {} was marked currently_booting in init",
                        patch.number
                    )
                    .as_ref(),
                ),
            ))?;
        }

        Ok(())
    })
    .map_err(|e| {
        shorebird_error!("Failed to clean up after a failed patch: {:?}", e);
        InitError::FailedToCleanUpFailedPatch
    })
}

/// Whether the auto-update flag is set to true in the config.
pub fn should_auto_update() -> anyhow::Result<bool> {
    with_config(|config| Ok(config.auto_update))
}

/// Synchronously checks for an update on the first non-null channel of:
///   1. `c_channel`
///   2. The channel specified in shorebird.yaml
///   3. The default "stable" channel
///
/// Returns true if an update is available for download. Will return false if the update is already
/// downloaded and ready to install.
pub fn check_for_downloadable_update(channel: Option<&str>) -> anyhow::Result<bool> {
    let client_id = with_state(|state| Ok(state.client_id()))?;

    let (request, url, request_fn) = with_config(|config| {
        let mut config = config.clone();

        match channel {
            Some(channel) => config.channel = channel.to_string(),
            None => {}
        }

        Ok((
            PatchCheckRequest::new(&config, &client_id),
            patches_check_url(&config.base_url),
            config.network_hooks.patch_check_request_fn,
        ))
    })?;

    let response = request_fn(&url, request)?;
    shorebird_debug!("Patch check response: {:?}", response);

    if let Some(rolled_back_patches) = response.rolled_back_patch_numbers {
        roll_back_patches_if_needed(rolled_back_patches)?;
    }

    if let Some(patch) = response.patch {
        match should_install_patch(patch.number)? {
            ShouldInstallPatchCheckResult::PatchOkToInstall => Ok(true),
            ShouldInstallPatchCheckResult::PatchKnownBad => Ok(false),
            ShouldInstallPatchCheckResult::PatchAlreadyInstalled => Ok(false),
        }
    } else {
        Ok(false)
    }
}

fn check_hash(path: &Path, expected_string: &str) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256}; // `Digest` is needed for `Sha256::new()`;

    let expected = hex::decode(expected_string).context("Invalid hash string from server.")?;

    // Based on guidance from:
    // <https://github.com/RustCrypto/hashes#hashing-readable-objects>

    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    // Check that the length from copy is the same as the file size?
    let hash = hasher.finalize();
    let hash_matches = hash.as_slice() == expected;
    // This is a common error for developers.  We could avoid it entirely
    // by sending the hash of `libapp.so` to the server and having the
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
    }
    shorebird_debug!("Hash match: {:?}", path);
    Ok(())
}

impl ReadSeek for Cursor<Vec<u8>> {}
impl ReadSeek for fs::File {}

// FIXME: these patch_base functions should move to platform-specific modules where they can all be tested.
#[cfg(any(target_os = "android", test))]
fn patch_base(config: &UpdateConfig) -> anyhow::Result<Box<dyn ReadSeek>> {
    let base_r = crate::android::open_base_lib(&config.libapp_path, "libapp.so")?;
    Ok(Box::new(base_r))
}

#[cfg(target_os = "ios")]
fn patch_base(config: &UpdateConfig) -> anyhow::Result<Box<dyn ReadSeek>> {
    config.file_provider.open()
}

#[cfg(all(not(test), not(target_os = "ios"), not(target_os = "android")))]
fn patch_base(config: &UpdateConfig) -> anyhow::Result<Box<dyn ReadSeek>> {
    let file = fs::File::open(&config.libapp_path)?;
    Ok(Box::new(file))
}

fn copy_update_config() -> anyhow::Result<UpdateConfig> {
    with_config(|config: &UpdateConfig| Ok(config.clone()))
}

// Callers must possess the Updater lock, but we don't care about the contents
// since they're empty.
fn update_internal(_: &UpdaterLockState, channel: Option<&str>) -> anyhow::Result<UpdateStatus> {
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

    let mut config = copy_update_config()?;
    if channel.is_some() {
        config.channel = channel.unwrap().to_string();
    }

    // We discard any events if we have more than 3 queued to make sure
    // we don't stall the client.
    let events = with_state(|state| Ok(state.copy_events(3)))?;
    for event in events {
        let result = crate::network::send_patch_event(event, &config);
        if let Err(err) = result {
            shorebird_error!("Failed to report event: {:?}", err);
        }
    }
    let request = with_mut_state(|state| {
        // This will clear any events which got queued between the time we
        // loaded the state now, but that's OK for now.
        let result = state.clear_events();
        if let Err(err) = result {
            shorebird_error!("Failed to clear events: {:?}", err);
        }
        // Update our outer state with the new state.
        Ok(PatchCheckRequest::new(&config, &state.client_id()))
    })?;

    // Check for update.
    let patch_check_request_fn = &(config.network_hooks.patch_check_request_fn);
    let response = patch_check_request_fn(&patches_check_url(&config.base_url), request)?;
    shorebird_info!("Patch check response: {:?}", response);

    if let Some(rolled_back_patches) = response.rolled_back_patch_numbers {
        roll_back_patches_if_needed(rolled_back_patches)?;
    }

    if !response.patch_available {
        return Ok(UpdateStatus::NoUpdate);
    }

    let patch = response.patch.ok_or(UpdateError::BadServerResponse)?;

    match should_install_patch(patch.number)? {
        ShouldInstallPatchCheckResult::PatchOkToInstall => {}
        ShouldInstallPatchCheckResult::PatchKnownBad => return Ok(UpdateStatus::UpdateIsBadPatch),
        ShouldInstallPatchCheckResult::PatchAlreadyInstalled => return Ok(UpdateStatus::NoUpdate),
    }

    let download_dir = PathBuf::from(&config.download_dir);
    let download_path = download_dir.join(patch.number.to_string());
    // Consider supporting allowing the system to download for us (e.g. iOS).
    download_to_path(&config.network_hooks, &patch.download_url, &download_path)?;

    let output_path = download_dir.join(format!("{}.full", patch.number));
    let patch_base_rs = patch_base(&config)?;
    inflate(&download_path, patch_base_rs, &output_path)?;

    // Check the hash before moving into place.
    check_hash(&output_path, &patch.hash).with_context(|| {
        format!(
            "This app reports version {}, but the binary is different from \
        the version {} that was submitted to Shorebird.",
            config.release_version, config.release_version
        )
    })?;

    // We're abusing the config lock as a UpdateState lock for now.
    // This makes it so we never try to write to the UpdateState file from
    // two threads at once. We could give UpdateState its own lock instead.
    with_mut_state(|state| {
        let patch_info = PatchInfo {
            path: output_path,
            number: patch.number,
        };
        // Move/state update should be "atomic" (it isn't today).
        state.install_patch(&patch_info, &patch.hash, patch.hash_signature.as_deref())?;
        shorebird_info!(
            "Patch {} successfully downloaded. It will be launched when the app next restarts.",
            patch.number
        );

        let client_id = state.client_id();
        std::thread::spawn(move || {
            let event = PatchEvent::new(
                &config,
                EventType::PatchDownload,
                patch.number,
                client_id,
                None,
            );
            let report_result = crate::network::send_patch_event(event, &config);
            if let Err(err) = report_result {
                shorebird_error!("Failed to report patch download: {:?}", err);
            }
        });

        // Should set some state to say the status is "update required" and that
        // we now have a different "next" version of the app from the current
        // booted version (patched or not).
        Ok(UpdateStatus::UpdateInstalled)
    })
}

fn roll_back_patches_if_needed(patch_numbers: Vec<usize>) -> anyhow::Result<()> {
    with_mut_state(|state| {
        for patch_number in patch_numbers {
            state.uninstall_patch(patch_number)?;
        }
        Ok(())
    })
}

fn should_install_patch(patch_number: usize) -> Result<ShouldInstallPatchCheckResult> {
    // Don't install a patch if it has previously failed to boot.
    let is_known_bad_patch = with_state(|state| Ok(state.is_known_bad_patch(patch_number)))?;
    if is_known_bad_patch {
        shorebird_info!(
            "Patch {} has previously failed to boot, skipping.",
            patch_number
        );
        return Ok(ShouldInstallPatchCheckResult::PatchKnownBad);
    }

    // If we already have the latest available patch downloaded, we don't need to download it again.
    let next_boot_patch = with_mut_state(|state| Ok(state.next_boot_patch()))?;
    if let Some(next_boot_patch) = next_boot_patch {
        if next_boot_patch.number == patch_number {
            shorebird_info!("Patch {} is already installed, skipping.", patch_number);
            return Ok(ShouldInstallPatchCheckResult::PatchAlreadyInstalled);
        }
    }

    Ok(ShouldInstallPatchCheckResult::PatchOkToInstall)
}

/// Synchronously checks for an update and downloads and installs it if available.
pub fn update(channel: Option<&str>) -> anyhow::Result<UpdateStatus> {
    with_updater_thread_lock(|lock_state| update_internal(lock_state, channel))
}

/// Given a path to a patch file, and a base file, apply the patch to the base
/// and write the result to the output path.
fn inflate<RS>(patch_path: &Path, base_r: RS, output_path: &Path) -> anyhow::Result<()>
where
    RS: Read + Seek,
{
    use comde::de::Decompressor;
    use comde::zstd::ZstdDecompressor;
    use std::io::{BufReader, BufWriter};

    // Open all our files first for error clarity.  Otherwise we might see
    // PipeReader/Writer errors instead of file open errors.
    shorebird_info!("Inflating patch from {:?}", patch_path);
    let compressed_patch_r = BufReader::new(
        fs::File::open(patch_path)
            .context(format!("Failed to open patch file: {:?}", patch_path))?,
    );
    let output_file_w = fs::File::create(output_path)?;

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
            shorebird_error!("Decompression thread failed: {}", err);
        }
    });

    // Do the patch, using the uncompressed patch data from the pipe.
    let mut fresh_r = bipatch::Reader::new(patch_r, base_r)?;

    // Write out the resulting patched file to the new location.
    let mut output_w = BufWriter::new(output_file_w);
    std::io::copy(&mut fresh_r, &mut output_w)?;
    shorebird_info!("Patch successfully applied to {:?}", output_path);
    Ok(())
}

/// Performs integrity checks on the next boot patch. If the patch fails these checks, the patch
/// will be deleted and the next boot patch will be set to the last successfully booted patch or
/// the base release if there is no last successfully booted patch.
///
/// Returns an error if the patch fails integrity checks.
pub fn validate_next_boot_patch() -> anyhow::Result<()> {
    with_mut_state(|state| state.validate_next_boot_patch())
}

/// The patch which will be run on next boot (which may still be the same
/// as the current boot).
/// This may be changed any time by:
///  1. `update()`
///  2. `start_update_thread()`
///  3. `report_launch_failure()`
pub fn next_boot_patch() -> anyhow::Result<Option<PatchInfo>> {
    with_mut_state(|state| Ok(state.next_boot_patch()))
}

/// The patch that was last successfully booted. If we're booting a patch for the first time, this
/// will be the previous patch (or None, if there was no previous patch) until the boot is
/// reported as successful.
pub fn current_boot_patch() -> anyhow::Result<Option<PatchInfo>> {
    with_state(|state| Ok(state.current_boot_patch()))
}

pub fn report_launch_start() -> anyhow::Result<()> {
    // We previously set the "current" patch the value of the "next" patch, but no longer
    // do so because the semantics have changed:
    //   current is now "last successfully booted patch"
    //   next is now "patch to boot next"
    shorebird_info!("Reporting launch start.");

    with_mut_state(|state| {
        if let Some(next_boot_patch) = state.next_boot_patch() {
            state.record_boot_start_for_patch(next_boot_patch.number)
        } else {
            Ok(())
        }
    })
}

/// Report that the current active path failed to launch.
/// This will mark the patch as bad and activate the next best patch.
pub fn report_launch_failure() -> anyhow::Result<()> {
    shorebird_info!("Reporting failed launch.");

    with_config(|config| {
        let mut state = UpdaterState::load_or_new_on_error(
            &config.storage_dir,
            &config.release_version,
            config.patch_public_key.as_deref(),
            config.patch_verification,
        );

        let patch = state.currently_booting_patch().ok_or(anyhow::Error::from(
            UpdateError::InvalidState("currently_booting_patch is None".to_string()),
        ))?;
        // Ignore the error here, we'll try to activate the next best patch
        // even if we fail to mark this one as bad (because it was already bad).
        let mark_result = state.record_boot_failure_for_patch(patch.number);
        if mark_result.is_err() {
            shorebird_error!("Failed to mark patch as bad: {:?}", mark_result);
        }
        let client_id = state.client_id();
        let event = PatchEvent::new(
            config,
            EventType::PatchInstallFailure,
            patch.number,
            client_id,
            Some(
                format!(
                    "Install failure reported from engine for patch {}",
                    patch.number
                )
                .as_ref(),
            ),
        );
        // Queue the failure event for later sending since right after this
        // function returns the Flutter engine is likely to abort().
        state.queue_event(event)
    })
}

pub fn report_launch_success() -> anyhow::Result<()> {
    shorebird_info!("Reporting successful launch.");

    with_config(|config| {
        // We can tell the UpdaterState that we have successfully booted from the "next" patch
        // and make that the "current" patch.
        let mut state = UpdaterState::load_or_new_on_error(
            &config.storage_dir,
            &config.release_version,
            config.patch_public_key.as_deref(),
            config.patch_verification,
        );

        let booting_patch = match state.currently_booting_patch() {
            Some(patch) => patch,

            // We didn't boot from a patch, so there's nothing to do.
            None => return Ok(()),
        };

        // Get the last successfully booted patch before we record the boot success.
        let maybe_previous_boot_patch = state.last_successfully_booted_patch();

        state.record_boot_success()?;

        // Check whether last_successfully_booted_patch has changed. If so, we should report a
        // PatchInstallSuccess event.
        if let (Some(previous_boot_patch), Some(current_boot_patch)) = (
            maybe_previous_boot_patch,
            state.last_successfully_booted_patch(),
        ) {
            // If we had previously booted from a patch and it has the same number as the
            // patch we just booted from, then we shouldn't report a patch install.
            if previous_boot_patch.number == current_boot_patch.number {
                return Ok(());
            }
        }

        let config_copy = config.clone();
        let client_id = state.client_id();
        std::thread::spawn(move || {
            let event = PatchEvent::new(
                &config_copy,
                EventType::PatchInstallSuccess,
                booting_patch.number,
                client_id,
                None,
            );
            let report_result = crate::network::send_patch_event(event, &config_copy);
            if let Err(err) = report_result {
                shorebird_error!("Failed to report successful patch install: {:?}", err);
            }
        });

        Ok(())
    })
}

/// This does not return status.  The only output is the change to the saved
/// cache. The Engine calls this during boot and it will check for an update
/// and install it if available.
pub fn start_update_thread() {
    std::thread::spawn(move || {
        let result = update(None);
        let status = match result {
            Ok(status) => status,
            Err(err) => {
                shorebird_error!("Update failed: {:?}", err);
                UpdateStatus::UpdateHadError
            }
        };
        shorebird_info!("Update thread finished with status: {}", status);
    });
}

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use std::{fs, thread, time::Duration};
    use tempdir::TempDir;

    use crate::{
        cache::UpdaterState,
        config::{testing_reset_config, with_config},
        events::EventType,
        network::{testing_set_network_hooks, NetworkHooks, PatchCheckResponse},
        test_utils::{install_fake_patch, write_fake_apk},
        time, with_state, ExternalFileProvider, Patch,
    };

    #[derive(Debug, Clone)]
    pub struct FakeExternalFileProvider {}
    impl ExternalFileProvider for FakeExternalFileProvider {
        fn open(&self) -> anyhow::Result<Box<dyn crate::ReadSeek>> {
            Ok(Box::new(std::io::Cursor::new(vec![])))
        }
    }

    pub fn init_for_testing(tmp_dir: &TempDir, base_url: Option<&str>) {
        testing_reset_config();
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
        let mut yaml = "app_id: 1234".to_string();
        if let Some(url) = base_url {
            yaml += &format!("\nbase_url: {}", url);
        }

        let libapp_path = tmp_dir
            .path()
            .join("lib/arch/libapp.so")
            .to_str()
            .unwrap()
            .to_string();

        crate::init(
            crate::AppConfig {
                app_storage_dir: cache_dir.clone(),
                code_cache_dir: cache_dir.clone(),
                release_version: "1.0.0+1".to_string(),
                original_libapp_paths: vec![libapp_path],
            },
            Box::new(FakeExternalFileProvider {}),
            &yaml,
        )
        .unwrap();
    }

    #[serial]
    #[test]
    fn subsequent_init_calls_do_not_update_config() {
        let tmp_dir = TempDir::new("example").unwrap();

        testing_reset_config();
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
        let mut yaml = "app_id: 1234".to_string();

        assert_eq!(
            crate::init(
                crate::AppConfig {
                    app_storage_dir: cache_dir.clone(),
                    code_cache_dir: cache_dir.clone(),
                    release_version: "1.0.0+1".to_string(),
                    original_libapp_paths: vec!["/dir/lib/arch/libapp.so".to_string()],
                },
                Box::new(FakeExternalFileProvider {}),
                &yaml,
            ),
            Ok(())
        );

        with_config(|config| {
            assert_eq!(config.app_id, "1234");
            Ok(())
        })
        .unwrap();

        // Attempt to init a second time with a different app_id.
        yaml = "app_id: 5678".to_string();
        assert_eq!(
            crate::init(
                crate::AppConfig {
                    app_storage_dir: cache_dir.clone(),
                    code_cache_dir: cache_dir.clone(),
                    release_version: "1.0.0+1".to_string(),
                    original_libapp_paths: vec!["/dir/lib/arch/libapp.so".to_string()],
                },
                Box::new(FakeExternalFileProvider {}),
                &yaml,
            ),
            Err(crate::InitError::AlreadyInitialized)
        );

        // Verify that the app_id is still the original value.
        with_config(|config| {
            assert_eq!(config.app_id, "1234");
            Ok(())
        })
        .unwrap();
    }

    #[serial]
    #[test]
    fn ignore_version_after_marked_bad() -> anyhow::Result<()> {
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, None);

        // Install a fake patch.
        install_fake_patch(1)?;
        assert!(crate::next_boot_patch()?.is_some());
        // pretend we booted from it
        crate::report_launch_start()?;
        crate::report_launch_success()?;
        assert!(crate::next_boot_patch()?.is_some());
        with_state(|state| {
            assert!(!state.is_known_bad_patch(1));
            Ok(())
        })?;
        // boot again, this time failing
        crate::report_launch_start()?;
        crate::report_launch_failure()?;
        // Technically might need to "reload"
        // ask for current patch (should get none).
        assert!(crate::next_boot_patch()?.is_none());
        with_state(|state| {
            assert!(state.is_known_bad_patch(1));
            Ok(())
        })?;

        Ok(())
    }

    #[serial]
    #[test]
    fn reports_patch_install_failure_if_patch_was_booting() -> anyhow::Result<()> {
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, None);

        install_fake_patch(1)?;
        with_config(|config| {
            let mut state = UpdaterState::load_or_new_on_error(
                &config.storage_dir,
                &config.release_version,
                config.patch_public_key.as_deref(),
                config.patch_verification,
            );
            assert_eq!(state.next_boot_patch().unwrap().number, 1);
            Ok(())
        })?;

        // Pretend we started to boot from it, but don't report success or failure.
        crate::report_launch_start()?;
        with_state(|state| {
            assert_eq!(state.currently_booting_patch().unwrap().number, 1);
            // We should have no queued events
            assert!(state.copy_events(1).is_empty());
            Ok(())
        })?;

        // Pretend we're starting the app a second time
        init_for_testing(&tmp_dir, None);

        with_state(|state| {
            assert!(state.currently_booting_patch().is_none());
            // We should now have a queued PatchInstallFailure event
            let events = state.copy_events(1);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].identifier, EventType::PatchInstallFailure);
            Ok(())
        })?;

        Ok(())
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
                    app_storage_dir: cache_dir.clone(),
                    code_cache_dir: cache_dir.clone(),
                    release_version: "1.0.0+1".to_string(),
                    original_libapp_paths: vec!["original_libapp_path".to_string()],
                },
                Box::new(FakeExternalFileProvider {}),
                "",
            ),
            Err(crate::InitError::InvalidArgument(
                "yaml".to_string(),
                "missing field `app_id`".to_string()
            ))
        );
    }

    #[serial]
    #[test]
    fn init_invalid_patch_verification() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
        let yaml = r#"
app_id: test_app
patch_verification: bogus_mode
"#;
        let result = crate::init(
            crate::AppConfig {
                app_storage_dir: cache_dir.clone(),
                code_cache_dir: cache_dir.clone(),
                release_version: "1.0.0+1".to_string(),
                original_libapp_paths: vec!["original_libapp_path".to_string()],
            },
            Box::new(FakeExternalFileProvider {}),
            yaml,
        );
        match result {
            Err(crate::InitError::InvalidArgument(field, msg)) => {
                assert_eq!(field, "yaml");
                assert!(
                    msg.contains("unknown variant"),
                    "Expected 'unknown variant' in error message, got: {}",
                    msg
                );
            }
            _ => panic!("Expected InvalidArgument error, got: {:?}", result),
        }
    }

    #[serial]
    #[test]
    fn reports_patch_download_on_update() -> anyhow::Result<()> {
        let mut server = mockito::Server::new();
        let download_url = format!("{}/patch/1", server.url());
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: 1,
                download_url: download_url.to_string(),
                // Generated by `string_patch "hello world" "hello tests"`
                hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
                hash_signature: None,
            }),
            rolled_back_patch_numbers: Some(vec![2]),
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let _ = server
            .mock("GET", "/patch/1")
            .with_status(200)
            .with_body(
                // Generated by `string_patch "hello world" "hello tests"`
                [
                    40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0,
                    0, 0, 0, 5, 116, 101, 115, 116, 115, 0,
                ],
            )
            .create();
        let event_mock = server
            .mock("POST", "/api/v1/patches/events")
            .with_status(201)
            .match_body(mockito::Matcher::PartialJsonString(
                r#"
                    {
                        "event": {
                            "app_id": "1234",
                            "type": "__patch_download__",
                            "patch_number": 1,
                            "release_version": "1.0.0+1"
                        }
                    }"#
                .to_string(),
            ))
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        // Install the base apk to allow the "downloaded" patch 1 to successfully inflate and install.
        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        let result = super::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        // This is gross.
        // Because we spawn a thread to report the patch download event, we need to give it time
        // to run before we can assert that the event was sent. The more correct way to do this
        // would be to obtain the join handle from the thread and wait for it to finish, but that
        // would require a change to the API of the `update` function, which seems worse.
        thread::sleep(Duration::from_millis(10));

        // Assert that the patch download event was sent.
        event_mock.expect(1).assert();

        Ok(())
    }

    #[serial]
    #[test]
    fn returns_update_installed_if_reporting_patch_download_fails() -> anyhow::Result<()> {
        let mut server = mockito::Server::new();
        let download_url = format!("{}/patch/1", server.url());
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: 1,
                download_url: download_url.to_string(),
                // Generated by `string_patch "hello world" "hello tests"`
                hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
                hash_signature: None,
            }),
            rolled_back_patch_numbers: Some(vec![2]),
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let _ = server
            .mock("GET", "/patch/1")
            .with_status(200)
            .with_body(
                // Generated by `string_patch "hello world" "hello tests"`
                [
                    40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0,
                    0, 0, 0, 5, 116, 101, 115, 116, 115, 0,
                ],
            )
            .create();
        let event_mock = server
            .mock("POST", "/api/v1/patches/events")
            .with_status(503)
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        // Install the base apk to allow the "downloaded" patch 1 to successfully inflate and install.
        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        let result = super::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        // This is gross.
        // Because we spawn a thread to report the patch download event, we need to give it time
        // to run before we can assert that the event was sent. The more correct way to do this
        // would be to obtain the join handle from the thread and wait for it to finish, but that
        // would require a change to the API of the `update` function, which seems worse.
        thread::sleep(Duration::from_millis(10));

        // Assert that the patch download event was sent.
        event_mock.expect(1).assert();

        Ok(())
    }

    #[serial]
    #[test]
    fn report_launch_result_with_no_current_patch() {
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, None);
        assert!(crate::report_launch_start().is_ok());
        assert_eq!(
            crate::report_launch_failure()
                .unwrap_err()
                .downcast::<crate::UpdateError>()
                .unwrap(),
            crate::UpdateError::InvalidState("currently_booting_patch is None".to_string())
        );
        assert!(crate::report_launch_success().is_ok());
    }

    #[serial]
    #[test]
    fn report_launch_success_with_patch() {
        use crate::cache::UpdaterState;
        use crate::config::with_config;
        let patch_number = 1;
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, None);

        // Install a fake patch.
        install_fake_patch(1).unwrap();

        // Pretend we booted from it.
        crate::report_launch_start().unwrap();
        super::report_launch_success().unwrap();

        with_config(|config| {
            let state = UpdaterState::load_or_new_on_error(
                &config.storage_dir,
                &config.release_version,
                config.patch_public_key.as_deref(),
                config.patch_verification,
            );
            assert_eq!(
                state.last_successfully_booted_patch().unwrap().number,
                patch_number
            );
            Ok(())
        })
        .unwrap();
    }

    #[serial]
    #[test]
    fn report_launch_failure_with_patch() {
        use crate::cache::UpdaterState;
        use crate::config::with_config;
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, None);

        // Install a fake patch.
        install_fake_patch(1).unwrap();

        // Pretend we fail to boot from it.
        crate::report_launch_start().unwrap();
        super::report_launch_failure().unwrap();

        with_config(|config| {
            let mut state = UpdaterState::load_or_new_on_error(
                &config.storage_dir,
                &config.release_version,
                config.patch_public_key.as_deref(),
                config.patch_verification,
            );
            // It's now bad.
            assert!(state.next_boot_patch().is_none());
            // And we've queued an event.
            let events = state.copy_events(1);
            assert_eq!(events.len(), 1);
            assert_eq!(
                events[0].identifier,
                crate::events::EventType::PatchInstallFailure
            );
            Ok(())
        })
        .unwrap();
    }

    #[serial]
    #[test]
    fn does_not_download_known_bad_patch() -> anyhow::Result<()> {
        let mut server = mockito::Server::new();
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(crate::network::Patch {
                number: 1,
                download_url: "download_url".to_string(),
                hash: "hash".to_string(),
                hash_signature: None,
            }),
            rolled_back_patch_numbers: None,
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        let mut updater_state = with_config(|config| {
            let mut state = UpdaterState::load_or_new_on_error(
                &config.storage_dir,
                &config.release_version,
                config.patch_public_key.as_deref(),
                config.patch_verification,
            );

            state.record_boot_failure_for_patch(1)?;

            Ok(state)
        })?;

        // Make sure we're starting with no next boot patch.
        assert!(updater_state.next_boot_patch().is_none());

        let result = super::update(None)?;

        // Ensure that we've skipped the known bad patch.
        assert_eq!(result, crate::UpdateStatus::UpdateIsBadPatch);
        assert!(updater_state.next_boot_patch().is_none());

        Ok(())
    }

    #[serial]
    #[test]
    fn does_not_download_already_installed_patch() -> anyhow::Result<()> {
        let patch_number = 1;
        let mut server = mockito::Server::new();
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: patch_number,
                hash: "#".to_string(),
                download_url: "download_url".to_string(),
                hash_signature: None,
            }),
            rolled_back_patch_numbers: None,
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        install_fake_patch(patch_number)?;

        let update_status = super::update(None)?;

        assert_eq!(update_status, crate::UpdateStatus::NoUpdate);

        Ok(())
    }

    #[serial]
    #[test]
    fn events_sent_during_update() {
        use crate::cache::UpdaterState;
        use crate::config::{current_arch, current_platform, with_config};
        use crate::events::{EventType, PatchEvent};
        use crate::network::PatchCheckResponse;

        let mut server = mockito::Server::new();
        let check_response = PatchCheckResponse {
            patch_available: false,
            patch: None,
            rolled_back_patch_numbers: None,
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let event_mock = server
            .mock("POST", "/api/v1/patches/events")
            .with_status(201)
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        with_config(|config| {
            let mut state = UpdaterState::load_or_new_on_error(
                &config.storage_dir,
                &config.release_version,
                config.patch_public_key.as_deref(),
                config.patch_verification,
            );
            let fail_event = PatchEvent {
                app_id: config.app_id.clone(),
                client_id: "client_id".to_string(),
                arch: current_arch().to_string(),
                identifier: EventType::PatchInstallFailure,
                patch_number: 1,
                platform: current_platform().to_string(),
                release_version: config.release_version.clone(),
                timestamp: time::unix_timestamp(),
                message: Some("Install failure reported from engine for patch 1".to_string()),
            };
            // Queue 5 events.
            assert!(state.queue_event(fail_event.clone()).is_ok());
            assert!(state.queue_event(fail_event.clone()).is_ok());
            assert!(state.queue_event(fail_event.clone()).is_ok());
            assert!(state.queue_event(fail_event.clone()).is_ok());
            assert!(state.queue_event(fail_event.clone()).is_ok());
            Ok(())
        })
        .unwrap();

        super::update(None).unwrap();
        // Only 3 events should have been sent.
        event_mock.expect(3).assert();

        with_config(|config| {
            let state = UpdaterState::load_or_new_on_error(
                &config.storage_dir,
                &config.release_version,
                config.patch_public_key.as_deref(),
                config.patch_verification,
            );
            // All 5 events should be cleared, even though only 3 were sent.
            assert_eq!(state.copy_events(10).len(), 0);
            Ok(())
        })
        .unwrap();
    }

    #[serial]
    #[test]
    fn no_config_lock_contention_when_waiting_for_patch_check() {
        static mut HAS_FINISHED_CONFIG: bool = false;

        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, None);

        // Set up the network hooks to sleep for 10 seconds on a patch check request
        let hooks = NetworkHooks {
            patch_check_request_fn: |_url, _request| {
                let patch_check_delay = std::time::Duration::from_secs(1);
                std::thread::sleep(patch_check_delay);

                // If we've obtained and released the config lock, this test has passed.
                if unsafe { HAS_FINISHED_CONFIG } {
                    return Ok(PatchCheckResponse {
                        patch_available: false,
                        patch: None,
                        rolled_back_patch_numbers: None,
                    });
                }

                // If we have not yet finished with the config lock, this test has failed.
                unreachable!("If the test has not terminated before this, set_config is likely being blocked by a patch check request, which should not happen");
            },
            download_file_fn: |_url| Ok([].to_vec()),
            report_event_fn: |_url, _event| Ok(()),
        };

        testing_set_network_hooks(
            hooks.patch_check_request_fn,
            hooks.download_file_fn,
            hooks.report_event_fn,
        );

        // Invoke check_for_update to kick off a patch check request
        let _ = std::thread::spawn(|| crate::check_for_downloadable_update(None));

        // Call with_config to get the config lock. This should complete before the patch check request is resolved.
        let config_thread = std::thread::spawn(|| with_config(|_| Ok(())));

        assert!(config_thread.join().is_ok());

        unsafe { HAS_FINISHED_CONFIG = true };

        // Don't wait for the patch check thread. This test should finish more or less immediately
        // if with_config isn't waiting for the patch check thread. If it is waiting, this test will
        // take patch_check_delay (defined above) to complete and fail due to the unreachable!() in
        // the patch check callback.
    }
}

#[cfg(test)]
mod rollback_tests {
    use anyhow::Result;
    use serial_test::serial;
    use tempdir::TempDir;

    use crate::{
        network::PatchCheckResponse,
        test_utils::{install_fake_patch, write_fake_apk},
    };

    use super::{
        report_launch_start, report_launch_success, tests::init_for_testing, with_mut_state, Patch,
    };

    #[serial]
    #[test]
    fn check_for_update_does_not_overwrite_good_next_patch() -> Result<()> {
        // Reported in https://discord.com/channels/1030243211995791380/1446453671414992977

        // Scenario:
        // - Patch 1 is installed
        // - Patches 2 and 3 are rolled back
        // - Patch 4 is live and available to download

        // Checking for a downloadable update sets the next boot patch to the latest booted, even
        // though the next boot patch is not rolled back.

        let mut server = mockito::Server::new();
        let download_url = format!("{}/patch/4", server.url());
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: 4,
                hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
                download_url: download_url.to_string(),
                hash_signature: None,
            }),
            rolled_back_patch_numbers: Some(vec![3, 2]),
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let _ = server
            .mock("POST", "/api/v1/patches/events")
            .with_status(201)
            .create();
        let _ = server
            .mock("GET", "/patch/4")
            .with_status(200)
            .with_body(
                // Generated by `string_patch "hello world" "hello tests"`
                [
                    40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0,
                    0, 0, 0, 5, 116, 101, 115, 116, 115, 0,
                ],
            )
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        install_fake_patch(1)?;

        report_launch_start()?;
        report_launch_success()?;

        let mut staging_update_check_result = crate::check_for_downloadable_update(None)?;
        assert!(staging_update_check_result);

        let stable_update_result: crate::UpdateStatus = crate::update(None)?;
        assert_eq!(stable_update_result, crate::UpdateStatus::UpdateInstalled);

        with_mut_state(|state| {
            let next_boot_patch = state.next_boot_patch();
            println!("next_boot_patch after update: {:?}", next_boot_patch);
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(4));
            Ok(())
        })?;

        staging_update_check_result = crate::check_for_downloadable_update(None)?;
        assert!(!staging_update_check_result);

        with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(4));
            Ok(())
        })?;

        Ok(())
    }

    #[serial]
    #[test]
    fn does_not_roll_back_when_rolled_back_patches_is_empty() -> Result<()> {
        let mut server = mockito::Server::new();
        let check_response = PatchCheckResponse {
            patch_available: false,
            patch: None,
            rolled_back_patch_numbers: Some(vec![]),
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        install_fake_patch(1)?;
        report_launch_start()?;
        report_launch_success()?;

        with_mut_state(|state| {
            assert_eq!(
                state.last_successfully_booted_patch().map(|p| p.number),
                Some(1)
            );
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        let update_result = crate::update(None);
        assert_eq!(update_result.unwrap(), crate::UpdateStatus::NoUpdate);

        Ok(())
    }

    /// If the next_boot_patch is rolled back, the updater should roll back to the release version
    /// if no other patches are available on disk.
    #[serial]
    #[test]
    fn rolls_back_from_current_patch_to_release() -> Result<()> {
        let mut server = mockito::Server::new();
        let check_response = PatchCheckResponse {
            patch_available: false,
            patch: None,
            rolled_back_patch_numbers: Some(vec![1]),
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        install_fake_patch(1)?;
        report_launch_start()?;
        report_launch_success()?;

        with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        crate::update(None)?;

        with_mut_state(|state| {
            assert!(state.next_boot_patch().is_none());
            Ok(())
        })?;

        Ok(())
    }

    /// If an older patch is provided by the patch check response, verify that we uninstall the
    /// rolled back patch and install the older patch specified by the patch check response.
    #[serial]
    #[test]
    fn rolls_back_to_previous_patch() -> Result<()> {
        let mut server = mockito::Server::new();
        let download_url = format!("{}/patch/1", server.url());
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: 1,
                download_url: download_url.to_string(),
                // Generated by `string_patch "hello world" "hello tests"`
                hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
                hash_signature: None,
            }),
            rolled_back_patch_numbers: Some(vec![2]),
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        let _ = server
            .mock("GET", "/patch/1")
            .with_status(200)
            .with_body(
                // Generated by `string_patch "hello world" "hello tests"`
                [
                    40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0,
                    0, 0, 0, 5, 116, 101, 115, 116, 115, 0,
                ],
            )
            .create();
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        // Install the base apk to allow the "downloaded" patch 1 to successfully inflate and install.
        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        // Install patch 2, pretend we're starting to boot from it, but don't report success or failure
        // to ensure we still have patch 1 on disk.
        install_fake_patch(2)?;
        report_launch_start()?;
        report_launch_success()?;

        with_mut_state(|state| {
            assert_eq!(
                state.last_successfully_booted_patch().map(|p| p.number),
                Some(2)
            );
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(2));
            Ok(())
        })?;

        let update_result = crate::update(None);
        assert_eq!(update_result.unwrap(), crate::UpdateStatus::UpdateInstalled);

        with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod check_for_downloadable_update_tests {
    use std::vec;

    use anyhow::Result;
    use serial_test::serial;
    use tempdir::TempDir;

    use crate::{
        network::PatchCheckResponse, report_launch_failure, report_launch_start,
        report_launch_success, test_utils::install_fake_patch, updater::tests::init_for_testing,
        with_mut_state,
    };

    use super::Patch;

    fn mock_server(
        available_patch_number: Option<usize>,
        rolled_back_patch_numbers: Option<Vec<usize>>,
    ) -> mockito::ServerGuard {
        let mut server = mockito::Server::new();
        let check_response = PatchCheckResponse {
            patch_available: available_patch_number.is_some(),
            patch: available_patch_number.map(|number| Patch {
                number,
                hash: "#".to_string(),
                download_url: "download_url".to_string(),
                hash_signature: None,
            }),
            rolled_back_patch_numbers,
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(check_response_body)
            .create();
        server
    }

    #[serial]
    #[test]
    fn returns_false_if_no_patch_is_available() -> Result<()> {
        let server = mock_server(None, None);
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        let is_update_available = crate::check_for_downloadable_update(None)?;
        assert!(!is_update_available);

        Ok(())
    }

    #[serial]
    #[test]
    fn returns_false_if_patch_is_already_installed() -> Result<()> {
        let patch_number = 1;
        let server = mock_server(Some(patch_number), None);
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        install_fake_patch(patch_number)?;

        let is_update_available = crate::check_for_downloadable_update(None)?;
        assert!(!is_update_available);

        Ok(())
    }

    #[serial]
    #[test]
    fn returns_false_if_patch_is_known_bad() -> Result<()> {
        let patch_number = 1;
        let server = mock_server(Some(patch_number), None);
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        install_fake_patch(patch_number)?;
        report_launch_start()?;
        report_launch_failure()?;

        let is_update_available = crate::check_for_downloadable_update(None)?;
        assert!(!is_update_available);

        Ok(())
    }

    #[serial]
    #[test]
    fn returns_true_if_patch_has_no_issues() -> Result<()> {
        let patch_number = 1;
        let server = mock_server(Some(patch_number), None);
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        let is_update_available = crate::check_for_downloadable_update(None)?;
        assert!(is_update_available);

        Ok(())
    }

    #[serial]
    #[test]
    fn rolls_back_patches_if_needed() -> Result<()> {
        let patch_number = 1;
        let server = mock_server(None, Some(vec![patch_number]));
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        install_fake_patch(patch_number)?;
        report_launch_start()?;
        report_launch_success()?;

        with_mut_state(|state| {
            assert_eq!(
                state.next_boot_patch().map(|p| p.number),
                Some(patch_number)
            );
            Ok(())
        })?;

        let is_update_available = crate::check_for_downloadable_update(None)?;
        assert!(!is_update_available);

        with_mut_state(|state| {
            assert!(state.next_boot_patch().map(|p| p.number).is_none());
            Ok(())
        })?;

        Ok(())
    }
}
