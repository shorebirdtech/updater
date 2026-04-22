// This file's job is to be the Rust API for the updater.

use std::fmt::{Debug, Display, Formatter};
use std::fs::{self};
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};

use crate::file_errors::{FileOperation, IoResultExt};
use anyhow::{bail, Context, Result};
use dyn_clone::DynClone;

use crate::cache::{PatchInfo, UpdaterState};
use crate::config::{set_config, with_config, UpdateConfig};
use crate::download_state::{self, DownloadState};
use crate::events::{EventType, PatchEvent};
use crate::logging::init_logging;
use crate::network::{download_to_path, patches_check_url, NetworkHooks, PatchCheckRequest};
use crate::updater_lock::{with_updater_thread_lock, UpdaterLockState};
use crate::yaml::YamlConfig;

#[cfg(test)]
// Expose testing_reset_config for integration tests.
pub use crate::config::testing_reset_config;
#[cfg(test)]
pub use crate::network::{DownloadToPathFn, Patch, PatchCheckRequestFn};

#[derive(Debug, PartialEq)]
pub enum UpdateStatus {
    NoUpdate,
    UpdateInstalled,
    UpdateHadError,
    UpdateIsBadPatch,
    // Another update was already in progress when this call was made. The
    // already-running update will continue; the caller did not start a new
    // one. This is a benign outcome, not an error.
    UpdateInProgress,
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
            UpdateStatus::UpdateInProgress => write!(f, "Update already in progress"),
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
            let elapsed_info = match state.boot_started_at() {
                Some(started_at) => {
                    let now = crate::time::unix_timestamp();
                    let elapsed_secs = now.saturating_sub(started_at);
                    format!("elapsed_secs={elapsed_secs}")
                }
                None => "elapsed_secs=unknown".to_string(),
            };

            let file_info = if patch.path.exists() {
                match std::fs::metadata(&patch.path) {
                    Ok(meta) => format!("file_ok=true,file_size={}", meta.len()),
                    Err(_) => "file_ok=false,file_unreadable".to_string(),
                }
            } else {
                "file_ok=false,file_missing".to_string()
            };

            let message = format!(
                "crash_recovery: patch {} failed to boot ({},{})",
                patch.number, elapsed_info, file_info
            );

            state.record_boot_failure_for_patch(patch.number)?;
            state.queue_event(PatchEvent::new(
                config,
                EventType::PatchInstallFailure,
                patch.number,
                state.client_id(),
                Some(&message),
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

        if let Some(channel) = channel {
            config.channel = channel.to_string()
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
    // Validate the expected string is a hex-encoded hash.
    hex::decode(expected_string).context("Invalid hash string from server.")?;
    let hash = crate::cache::hash_file(path)
        .with_context(|| format!("Failed to hash file: {:?}", path))?;
    let hash_matches = hash == expected_string;
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
            hash
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
    let file = fs::File::open(&config.libapp_path)
        .with_file_context(FileOperation::ReadFile, &config.libapp_path)?;
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
    if let Some(channel) = channel {
        config.channel = channel.to_string();
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

    shorebird_info!(
        "Downloading patch {} for app {} (version {})",
        patch.number,
        config.app_id,
        config.release_version
    );
    let download_dir = PathBuf::from(&config.download_dir);
    let download_path = download_dir.join(patch.number.to_string());

    // Compute resume offset (checks sidecar for matching URL/patch/hash).
    let resume_from = compute_resume_offset(
        &download_path,
        &patch.download_url,
        patch.number,
        &patch.hash,
    );

    // Ensure the download directory exists.
    std::fs::create_dir_all(&download_dir)
        .with_file_context(FileOperation::CreateDir, &download_dir)?;

    // Clean up any orphaned files in the download directory. We own this
    // directory entirely, so anything that isn't for the current patch is
    // stale (e.g. from a prior patch number, a crashed inflate, or a
    // partial download for a patch that's since been replaced).
    clean_download_dir(&download_dir, patch.number);

    // Write sidecar *before* downloading so we can resume on crash.
    let dl_state = DownloadState {
        url: patch.download_url.clone(),
        patch_number: patch.number,
        expected_size: None,
        expected_hash: patch.hash.clone(),
    };
    download_state::write_download_state(&download_path, &dl_state)?;

    // Consider supporting allowing the system to download for us (e.g. iOS).
    let dl_result = download_to_path(
        &config.network_hooks,
        &patch.download_url,
        &download_path,
        resume_from,
    )?;

    // Update sidecar with the now-known total size.
    // content_length is already the total file size (from Content-Range for
    // 206, or Content-Length for 200).
    let dl_state = DownloadState {
        expected_size: dl_result.content_length,
        ..dl_state
    };
    download_state::write_download_state(&download_path, &dl_state)?;

    // Validate download size if Content-Length was provided.
    if let Some(expected) = dl_state.expected_size {
        if dl_result.total_bytes != expected {
            // Corrupted — clean up so next attempt starts fresh.
            cleanup_download_artifacts(&download_path);
            bail!(
                "Download size mismatch: expected {} bytes, got {}",
                expected,
                dl_result.total_bytes
            );
        }
    }

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

        // Clean up download artifacts now that installation succeeded.
        cleanup_download_artifacts(&download_path);

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

/// Determines how many bytes of a prior partial download we can resume from.
/// Returns 0 if we should start fresh.
fn compute_resume_offset(
    download_path: &Path,
    url: &str,
    patch_number: usize,
    expected_hash: &str,
) -> u64 {
    // Check for a sidecar file describing a prior download attempt.
    let prior_state = match download_state::read_download_state(download_path) {
        Ok(Some(state)) => state,
        _ => return 0,
    };

    // Only resume if URL, patch number, and hash all match. The hash check
    // catches the case where a patch is deleted and re-added with the same
    // number — the URL might stay the same but the content differs.
    if prior_state.url != url
        || prior_state.patch_number != patch_number
        || prior_state.expected_hash != expected_hash
    {
        shorebird_info!("Download state mismatch, starting fresh.");
        return 0;
    }

    // Check that the partial file exists and has some content.
    match std::fs::metadata(download_path) {
        Ok(meta) if meta.len() > 0 => {
            shorebird_info!("Resuming download from byte {}", meta.len());
            meta.len()
        }
        _ => 0,
    }
}

/// Removes everything in `download_dir` except files belonging to
/// `current_patch_number`. We own this directory entirely, so anything
/// unrecognized or from a different patch number is safe to delete.
fn clean_download_dir(download_dir: &Path, current_patch_number: usize) {
    let entries = match fs::read_dir(download_dir) {
        Ok(entries) => entries,
        Err(_) => return, // Directory may not exist yet.
    };

    let current_prefix = current_patch_number.to_string();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Keep files that belong to the current patch:
        //   "{number}", "{number}.full", "{number}.download.json"
        if name == current_prefix
            || name == format!("{current_prefix}.full")
            || name == format!("{current_prefix}.download.json")
        {
            continue;
        }

        // Everything else is an orphan — delete it.
        let path = entry.path();
        if path.is_file() {
            if let Err(e) = fs::remove_file(&path) {
                shorebird_error!("Failed to clean up orphaned file {:?}: {:?}", path, e);
            } else {
                shorebird_info!("Cleaned up orphaned download file: {:?}", path);
            }
        }
    }
}

/// Removes the compressed download file and its sidecar after a successful
/// install.
fn cleanup_download_artifacts(download_path: &Path) {
    if let Err(e) = download_state::delete_download_state(download_path) {
        shorebird_error!("Failed to delete download sidecar: {:?}", e);
    }
    if download_path.exists() {
        if let Err(e) = std::fs::remove_file(download_path) {
            shorebird_error!("Failed to delete download file: {:?}", e);
        }
    }
}

/// Synchronously checks for an update and downloads and installs it if available.
pub fn update(channel: Option<&str>) -> anyhow::Result<UpdateStatus> {
    match with_updater_thread_lock(|lock_state| update_internal(lock_state, channel)) {
        Ok(status) => Ok(status),
        Err(e) => {
            // "Another update is already running" is a benign outcome — the
            // in-progress update (typically the automatic updater thread) will
            // continue on its own. Surface it as a non-error status so callers
            // that monitor `update()` exceptions do not see it as a failure.
            if matches!(
                e.downcast_ref::<UpdateError>(),
                Some(UpdateError::UpdateAlreadyInProgress)
            ) {
                Ok(UpdateStatus::UpdateInProgress)
            } else {
                Err(e)
            }
        }
    }
}

/// The first 4 bytes of any zstd compressed frame.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Validates that a downloaded patch file is a non-empty, valid zstd archive.
fn validate_compressed_patch(patch_path: &Path) -> anyhow::Result<()> {
    let metadata =
        fs::metadata(patch_path).with_file_context(FileOperation::GetMetadata, patch_path)?;
    let size = metadata.len();
    if size == 0 {
        bail!("Downloaded patch file is empty: {:?}", patch_path);
    }
    // A valid zstd frame is at least 4 bytes (magic number).
    if size < 4 {
        bail!(
            "Downloaded patch file is too small ({} bytes) to be a valid zstd archive: {:?}",
            size,
            patch_path
        );
    }
    let mut file =
        fs::File::open(patch_path).with_file_context(FileOperation::ReadFile, patch_path)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .with_file_context(FileOperation::ReadFile, patch_path)?;
    if magic != ZSTD_MAGIC {
        bail!(
            "Downloaded patch file does not have valid zstd magic bytes \
            (expected {:02x?}, got {:02x?}). The download may be corrupt: {:?}",
            ZSTD_MAGIC,
            magic,
            patch_path
        );
    }
    Ok(())
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

    // Validate the compressed patch before attempting decompression.
    validate_compressed_patch(patch_path)?;

    // Open all our files first for error clarity.  Otherwise we might see
    // PipeReader/Writer errors instead of file open errors.
    shorebird_info!("Inflating patch from {:?}", patch_path);
    let compressed_patch_r = BufReader::new(
        fs::File::open(patch_path).with_file_context(FileOperation::ReadFile, patch_path)?,
    );
    let output_file_w =
        fs::File::create(output_path).with_file_context(FileOperation::CreateFile, output_path)?;

    // Set up a pipe to connect the writing from the decompression thread
    // to the reading of the decompressed patch data on this thread.
    let (patch_r, patch_w) = pipe::pipe();

    let decompress = ZstdDecompressor::new();
    // Spawn a thread to run the decompression in parallel to the patching.
    // decompress.copy will block on the pipe being full (I think) and then
    // when it returns the thread will exit.
    let decompress_handle =
        std::thread::spawn(move || decompress.copy(compressed_patch_r, patch_w));

    // Do the patch, using the uncompressed patch data from the pipe.
    let mut fresh_r =
        bipatch::Reader::new(patch_r, base_r).context("Failed to initialize patch reader")?;

    // Write out the resulting patched file to the new location.
    let mut output_w = BufWriter::new(output_file_w);
    let patch_result = std::io::copy(&mut fresh_r, &mut output_w)
        .with_file_context(FileOperation::WriteFile, output_path);

    // IMPORTANT: Drop the reader side of the pipe before joining the
    // decompression thread. The pipe has a bounded buffer — if patching
    // failed, the decompression thread may be blocked on a write waiting
    // for the reader to drain data. Without this drop, join() below would
    // wait for the decompression thread forever (deadlock). Dropping the
    // reader causes the decompression thread's write to fail with a
    // broken-pipe error, allowing it to exit so join() can return.
    drop(fresh_r);

    // Always join the decompression thread to get its result.
    let decompress_result = decompress_handle
        .join()
        .map_err(|_| anyhow::anyhow!("Decompression thread panicked"))?;

    // If decompression failed, report that as the primary error since it is
    // the root cause (the patching thread fails as a side-effect when the
    // pipe writer is dropped).
    if let Err(decompress_err) = decompress_result {
        return Err(decompress_err).context("Decompression of patch failed");
    }

    // If decompression succeeded but patching failed, report the patch error.
    patch_result?;

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
        let message = format!(
            "engine_report: patch {} failed to launch",
            patch.number
        );
        let event = PatchEvent::new(
            config,
            EventType::PatchInstallFailure,
            patch.number,
            client_id,
            Some(&message),
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
    use std::path::Path;
    use std::{fs, thread, time::Duration};
    use tempfile::TempDir;

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
        let tmp_dir = TempDir::new().unwrap();

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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();

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
        let tmp_dir = TempDir::new().unwrap();
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
                "missing required field: app_id".to_string()
            ))
        );
    }

    #[serial]
    #[test]
    fn init_invalid_patch_verification() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();
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
                    msg.contains("invalid value for patch_verification"),
                    "Expected 'invalid value for patch_verification' in error message, got: {}",
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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

    #[test]
    fn validate_compressed_patch_rejects_empty_file() {
        let tmp_dir = TempDir::new().unwrap();
        let patch_path = tmp_dir.path().join("empty.patch");
        fs::write(&patch_path, b"").unwrap();

        let err = super::validate_compressed_patch(&patch_path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty"), "Expected 'empty' in error: {}", err);
    }

    #[test]
    fn validate_compressed_patch_rejects_too_small_file() {
        let tmp_dir = TempDir::new().unwrap();
        let patch_path = tmp_dir.path().join("small.patch");
        fs::write(&patch_path, b"abc").unwrap();

        let err = super::validate_compressed_patch(&patch_path)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("too small"),
            "Expected 'too small' in error: {}",
            err
        );
    }

    #[test]
    fn validate_compressed_patch_rejects_bad_magic() {
        let tmp_dir = TempDir::new().unwrap();
        let patch_path = tmp_dir.path().join("bad_magic.patch");
        fs::write(&patch_path, b"not_zstd_data_here").unwrap();

        let err = super::validate_compressed_patch(&patch_path)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("valid zstd magic bytes"),
            "Expected 'valid zstd magic bytes' in error: {}",
            err
        );
    }

    #[test]
    fn validate_compressed_patch_accepts_valid_zstd_magic() {
        let tmp_dir = TempDir::new().unwrap();
        let patch_path = tmp_dir.path().join("valid.patch");
        // Write zstd magic bytes followed by some data
        let mut data = vec![0x28, 0xB5, 0x2F, 0xFD];
        data.extend_from_slice(b"some_compressed_data");
        fs::write(&patch_path, &data).unwrap();

        assert!(super::validate_compressed_patch(&patch_path).is_ok());
    }

    #[test]
    fn inflate_fails_with_corrupt_zstd_data() {
        let tmp_dir = TempDir::new().unwrap();
        let patch_path = tmp_dir.path().join("corrupt.patch");
        // Valid zstd magic but garbage content — passes validation but
        // fails during decompression or patch reading.
        let mut data = vec![0x28, 0xB5, 0x2F, 0xFD];
        data.extend_from_slice(b"this is not valid zstd compressed data");
        fs::write(&patch_path, &data).unwrap();

        let output_path = tmp_dir.path().join("output");
        let base = std::io::Cursor::new(b"base content".to_vec());

        let err = super::inflate(&patch_path, base, &output_path).unwrap_err();
        let err_chain = format!("{:#}", err);
        // The error should come from either decompression or patch init —
        // not the old cryptic "pipe reader has been dropped" message.
        assert!(
            err_chain.contains("Decompression of patch failed")
                || err_chain.contains("Failed to initialize patch reader"),
            "Expected decompression or patch init error, got: {}",
            err_chain
        );
    }

    #[test]
    fn inflate_reports_decompression_error_as_primary() {
        use comde::com::Compressor;
        use comde::zstd::ZstdCompressor;

        let tmp_dir = TempDir::new().unwrap();

        // Build a fake uncompressed patch that starts with a valid bipatch
        // header (magic 0xB1DF, version 0x1000 as little-endian u32s) so
        // that bipatch::Reader::new succeeds.
        let mut uncompressed = Vec::new();
        uncompressed.extend_from_slice(&0xB1DFu32.to_le_bytes()); // bipatch magic
        uncompressed.extend_from_slice(&0x1000u32.to_le_bytes()); // bipatch version
        uncompressed.extend_from_slice(&vec![0u8; 1024]); // padding

        // Compress the valid data into one complete zstd frame.
        let mut compressed = std::io::Cursor::new(Vec::new());
        let compressor = ZstdCompressor::new();
        compressor
            .compress(&mut compressed, &mut std::io::Cursor::new(&uncompressed))
            .unwrap();
        let mut compressed = compressed.into_inner();

        // Append a second, corrupted zstd frame. The decompressor will
        // successfully decompress the first frame (delivering the bipatch
        // header so Reader::new succeeds), then fail on this corrupt frame.
        compressed.extend_from_slice(&[0x28, 0xB5, 0x2F, 0xFD]); // zstd magic
        compressed.extend_from_slice(&[0xFF; 64]); // garbage

        let patch_path = tmp_dir.path().join("corrupt_frame.patch");
        fs::write(&patch_path, &compressed).unwrap();

        let output_path = tmp_dir.path().join("output");
        let base = std::io::Cursor::new(b"base content".to_vec());

        let err = super::inflate(&patch_path, base, &output_path).unwrap_err();
        let err_chain = format!("{:#}", err);
        assert!(
            err_chain.contains("Decompression of patch failed"),
            "Expected decompression error as primary cause, got: {}",
            err_chain
        );
    }

    #[test]
    fn inflate_rejects_invalid_magic() {
        let tmp_dir = TempDir::new().unwrap();
        let patch_path = tmp_dir.path().join("bad.patch");
        fs::write(&patch_path, b"not zstd at all").unwrap();

        let output_path = tmp_dir.path().join("output");
        let base = std::io::Cursor::new(b"base content".to_vec());

        let err = super::inflate(&patch_path, base, &output_path)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("valid zstd magic bytes"),
            "Expected validation error, got: {}",
            err
        );
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
                message: Some("engine_report: patch 1 failed to launch".to_string()),
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

    #[test]
    fn compute_resume_offset_no_sidecar() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        fs::create_dir_all(download_path.parent().unwrap()).unwrap();

        // No sidecar, no partial file → fresh download.
        assert_eq!(
            super::compute_resume_offset(&download_path, "http://example.com/patch", 1, "abc123"),
            0
        );
    }

    #[test]
    fn compute_resume_offset_matching_sidecar() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        fs::create_dir_all(download_path.parent().unwrap()).unwrap();

        // Write partial file.
        fs::write(&download_path, vec![0u8; 500]).unwrap();

        // Write matching sidecar.
        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://example.com/patch".to_string(),
                patch_number: 1,
                expected_size: Some(1000),
                expected_hash: "abc123".to_string(),
            },
        )
        .unwrap();

        // Should resume from 500 bytes.
        assert_eq!(
            super::compute_resume_offset(&download_path, "http://example.com/patch", 1, "abc123"),
            500
        );
    }

    #[test]
    fn compute_resume_offset_mismatched_url() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        fs::create_dir_all(download_path.parent().unwrap()).unwrap();

        fs::write(&download_path, vec![0u8; 500]).unwrap();

        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://example.com/old-patch".to_string(),
                patch_number: 1,
                expected_size: None,
                expected_hash: "abc123".to_string(),
            },
        )
        .unwrap();

        // Different URL → fresh download.
        assert_eq!(
            super::compute_resume_offset(
                &download_path,
                "http://example.com/new-patch",
                1,
                "abc123"
            ),
            0
        );
    }

    #[test]
    fn compute_resume_offset_mismatched_hash() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        fs::create_dir_all(download_path.parent().unwrap()).unwrap();

        fs::write(&download_path, vec![0u8; 500]).unwrap();

        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://example.com/patch".to_string(),
                patch_number: 1,
                expected_size: None,
                expected_hash: "hash_old".to_string(),
            },
        )
        .unwrap();

        // Same URL but different hash (patch was re-created) → fresh download.
        assert_eq!(
            super::compute_resume_offset(&download_path, "http://example.com/patch", 1, "hash_new"),
            0
        );
    }

    #[test]
    fn compute_resume_offset_mismatched_patch_number() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        fs::create_dir_all(download_path.parent().unwrap()).unwrap();

        fs::write(&download_path, vec![0u8; 500]).unwrap();

        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://example.com/patch".to_string(),
                patch_number: 1,
                expected_size: None,
                expected_hash: "abc123".to_string(),
            },
        )
        .unwrap();

        // Same URL but different patch number → fresh download.
        assert_eq!(
            super::compute_resume_offset(&download_path, "http://example.com/patch", 2, "abc123"),
            0
        );
    }

    #[test]
    fn compute_resume_offset_corrupt_sidecar() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        fs::create_dir_all(download_path.parent().unwrap()).unwrap();

        fs::write(&download_path, vec![0u8; 500]).unwrap();

        // Write garbage to the sidecar file.
        let sidecar = crate::download_state::sidecar_path(&download_path);
        fs::write(&sidecar, "not valid json").unwrap();

        // Corrupt sidecar → fresh download.
        assert_eq!(
            super::compute_resume_offset(&download_path, "http://example.com/patch", 1, "abc123"),
            0
        );
    }

    #[test]
    fn compute_resume_offset_empty_file() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        fs::create_dir_all(download_path.parent().unwrap()).unwrap();

        // Write empty file.
        fs::write(&download_path, []).unwrap();

        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://example.com/patch".to_string(),
                patch_number: 1,
                expected_size: None,
                expected_hash: "abc123".to_string(),
            },
        )
        .unwrap();

        // Empty file → fresh download.
        assert_eq!(
            super::compute_resume_offset(&download_path, "http://example.com/patch", 1, "abc123"),
            0
        );
    }

    #[test]
    fn cleanup_download_artifacts_removes_file_and_sidecar() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        fs::create_dir_all(download_path.parent().unwrap()).unwrap();

        fs::write(&download_path, b"partial data").unwrap();
        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://example.com/patch".to_string(),
                patch_number: 1,
                expected_size: None,
                expected_hash: "abc123".to_string(),
            },
        )
        .unwrap();

        let sidecar = crate::download_state::sidecar_path(&download_path);
        assert!(download_path.exists());
        assert!(sidecar.exists());

        super::cleanup_download_artifacts(&download_path);

        assert!(!download_path.exists());
        assert!(!sidecar.exists());
    }

    #[test]
    fn cleanup_download_artifacts_noop_when_missing() {
        let tmp = TempDir::new().unwrap();
        let download_path = tmp.path().join("downloads/1");
        // Should not panic when files don't exist.
        super::cleanup_download_artifacts(&download_path);
    }

    #[test]
    fn clean_download_dir_removes_orphans_keeps_current() {
        let tmp = TempDir::new().unwrap();
        let download_dir = tmp.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();

        // Files for current patch (number 3) — should be kept.
        fs::write(download_dir.join("3"), b"compressed").unwrap();
        fs::write(download_dir.join("3.full"), b"inflated").unwrap();
        fs::write(download_dir.join("3.download.json"), b"{}").unwrap();

        // Files for old patches — should be deleted.
        fs::write(download_dir.join("1"), b"old compressed").unwrap();
        fs::write(download_dir.join("1.full"), b"old inflated").unwrap();
        fs::write(download_dir.join("1.download.json"), b"{}").unwrap();
        fs::write(download_dir.join("2"), b"old compressed").unwrap();

        // Unrecognized file — should be deleted.
        fs::write(download_dir.join("garbage.tmp"), b"junk").unwrap();

        super::clean_download_dir(&download_dir, 3);

        // Current patch files preserved.
        assert!(download_dir.join("3").exists());
        assert!(download_dir.join("3.full").exists());
        assert!(download_dir.join("3.download.json").exists());

        // Old and unrecognized files removed.
        assert!(!download_dir.join("1").exists());
        assert!(!download_dir.join("1.full").exists());
        assert!(!download_dir.join("1.download.json").exists());
        assert!(!download_dir.join("2").exists());
        assert!(!download_dir.join("garbage.tmp").exists());
    }

    #[test]
    fn clean_download_dir_noop_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let download_dir = tmp.path().join("nonexistent");
        // Should not panic.
        super::clean_download_dir(&download_dir, 1);
    }

    #[serial]
    #[test]
    fn successful_update_cleans_up_download_artifacts() -> anyhow::Result<()> {
        let mut server = mockito::Server::new();
        let download_url = format!("{}/patch/1", server.url());
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: 1,
                download_url: download_url.to_string(),
                hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
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
        let _ = server
            .mock("POST", "/api/v1/patches/events")
            .with_status(201)
            .create();
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        let result = super::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        // After successful install, compressed download and sidecar should be cleaned up.
        let download_path = tmp_dir.path().join("downloads/1");
        let sidecar_path = crate::download_state::sidecar_path(&download_path);
        assert!(
            !download_path.exists(),
            "compressed download should be deleted"
        );
        assert!(!sidecar_path.exists(), "sidecar should be deleted");

        Ok(())
    }

    #[serial]
    #[test]
    fn update_fails_on_download_size_mismatch() -> anyhow::Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        crate::test_utils::write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(Patch {
                        number: 1,
                        hash: "abc123".to_string(),
                        download_url: "http://example.com/patch/1".to_string(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| {
                // Write 10 bytes but claim the server said 9999.
                let data = vec![0u8; 10];
                std::fs::write(dest, &data)?;
                Ok(crate::network::DownloadResult {
                    total_bytes: 10,
                    content_length: Some(9999),
                })
            },
            |_url, _event| Ok(()),
        );

        let result = super::update(None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Download size mismatch"),
            "Expected size mismatch error, got: {err}"
        );

        // Verify artifacts were cleaned up after the mismatch.
        let download_path = tmp_dir.path().join("downloads/1");
        let sidecar_path = crate::download_state::sidecar_path(&download_path);
        assert!(!download_path.exists(), "download should be cleaned up");
        assert!(!sidecar_path.exists(), "sidecar should be cleaned up");

        Ok(())
    }

    #[serial]
    #[test]
    fn update_succeeds_when_content_length_unknown() -> anyhow::Result<()> {
        // When the server doesn't provide Content-Length (content_length: None),
        // the size check should be skipped and the update should proceed.
        let mut server = mockito::Server::new();
        let download_url = format!("{}/patch/1", server.url());
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: 1,
                download_url: download_url.to_string(),
                hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
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
        // Serve without Content-Length by using chunked transfer.
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
        let _ = server
            .mock("POST", "/api/v1/patches/events")
            .with_status(201)
            .create();
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        crate::test_utils::write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        let result = super::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        Ok(())
    }

    #[serial]
    #[test]
    fn update_resumes_partial_download() -> anyhow::Result<()> {
        // This test verifies that if a partial download + sidecar exist from a
        // prior attempt, the updater sends a Range header to resume.
        let mut server = mockito::Server::new();
        let download_url = format!("{}/patch/1", server.url());
        // Generated by `string_patch "hello world" "hello tests"`
        let patch_bytes: Vec<u8> = vec![
            40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0, 0, 0, 0,
            5, 116, 101, 115, 116, 115, 0,
        ];
        let split_at = 10;
        let first_part = &patch_bytes[..split_at];
        let second_part = &patch_bytes[split_at..];

        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: 1,
                download_url: download_url.to_string(),
                hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
                hash_signature: None,
            }),
            rolled_back_patch_numbers: None,
        };
        let check_response_body = serde_json::to_string(&check_response).unwrap();
        let _ = server
            .mock("POST", "/api/v1/patches/check")
            .with_status(200)
            .with_body(&check_response_body)
            .create();
        // Serve only the remaining bytes with 206 and Content-Range.
        let _ = server
            .mock("GET", "/patch/1")
            .match_header("Range", format!("bytes={split_at}-").as_str())
            .with_status(206)
            .with_header(
                "Content-Range",
                &format!(
                    "bytes {}-{}/{}",
                    split_at,
                    patch_bytes.len() - 1,
                    patch_bytes.len()
                ),
            )
            .with_body(second_part)
            .create();
        let _ = server
            .mock("POST", "/api/v1/patches/events")
            .with_status(201)
            .create();

        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        // Simulate a prior partial download: write the first 10 bytes + sidecar.
        let download_dir = tmp_dir.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        let download_path = download_dir.join("1");
        fs::write(&download_path, first_part).unwrap();
        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: download_url.to_string(),
                patch_number: 1,
                expected_size: None,
                expected_hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
            },
        )
        .unwrap();

        // Run update — should resume from byte 10.
        let result = super::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        // Verify the patched file was written correctly.
        crate::updater::with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().unwrap().number, 1);
            Ok(())
        })?;

        Ok(())
    }

    #[serial]
    #[test]
    fn update_starts_fresh_when_url_changes() -> anyhow::Result<()> {
        let mut server = mockito::Server::new();
        let download_url = format!("{}/patch/1", server.url());
        let check_response = PatchCheckResponse {
            patch_available: true,
            patch: Some(Patch {
                number: 1,
                download_url: download_url.to_string(),
                hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45"
                    .to_string(),
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
        // Full download (200), no Range header expected since URL changed.
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
        let _ = server
            .mock("POST", "/api/v1/patches/events")
            .with_status(201)
            .create();
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, Some(&server.url()));

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        // Simulate prior partial download with a DIFFERENT URL.
        let download_dir = tmp_dir.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        let download_path = download_dir.join("1");
        fs::write(&download_path, b"stale data from old url").unwrap();
        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://old-cdn.example.com/patch/1".to_string(),
                patch_number: 1,
                expected_size: None,
                expected_hash: "hash_old".to_string(),
            },
        )
        .unwrap();

        let result = super::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        Ok(())
    }

    #[serial]
    #[test]
    fn no_config_lock_contention_when_waiting_for_patch_check() {
        static mut HAS_FINISHED_CONFIG: bool = false;

        let tmp_dir = TempDir::new().unwrap();
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
            download_to_path_fn: |_url, _dest: &Path, _resume_from: u64| {
                Ok(crate::network::DownloadResult {
                    total_bytes: 0,
                    content_length: None,
                })
            },
            report_event_fn: |_url, _event| Ok(()),
        };

        testing_set_network_hooks(
            hooks.patch_check_request_fn,
            hooks.download_to_path_fn,
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
    use tempfile::TempDir;

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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
    use tempfile::TempDir;

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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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

/// Tests for state corruption and crash recovery scenarios.
///
/// These test what happens when the updater encounters corrupt or missing
/// state files — the most important "fail-open" safety net.
#[cfg(test)]
mod state_recovery_tests {
    use anyhow::Result;
    use serial_test::serial;
    use tempfile::TempDir;

    use crate::{
        report_launch_start, report_launch_success, test_utils::install_fake_patch,
        updater::tests::init_for_testing, with_mut_state, with_state,
    };

    /// When patches_state.json is corrupt, PatchManager should fall back to
    /// default (empty) state. The updater should still function — no panic,
    /// no boot from stale patch.
    #[serial]
    #[test]
    fn corrupt_patches_state_resets_to_empty() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        // Install a patch and successfully boot it.
        install_fake_patch(1)?;
        report_launch_start()?;
        report_launch_success()?;

        with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        // Corrupt the patches_state.json file.
        let patches_state_path = tmp_dir.path().join("patches_state.json");
        assert!(
            patches_state_path.exists(),
            "patches_state.json should exist before we corrupt it"
        );
        std::fs::write(&patches_state_path, "{{{{not json at all")?;

        // Reinitialize — should recover gracefully.
        init_for_testing(&tmp_dir, None);

        with_mut_state(|state| {
            // Corrupt state means we lose knowledge of the patch.
            assert!(
                state.next_boot_patch().is_none(),
                "Expected no next_boot_patch after state corruption"
            );
            Ok(())
        })?;

        Ok(())
    }

    /// When patches_state.json is deleted but patch artifacts remain on disk,
    /// the updater should not try to boot from orphaned artifacts.
    #[serial]
    #[test]
    fn missing_patches_state_with_artifacts_on_disk() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        install_fake_patch(1)?;
        report_launch_start()?;
        report_launch_success()?;

        // Delete patches_state.json but leave artifacts.
        let patches_state_path = tmp_dir.path().join("patches_state.json");
        std::fs::remove_file(&patches_state_path)?;

        // Reinitialize.
        init_for_testing(&tmp_dir, None);

        with_mut_state(|state| {
            // Without state, we shouldn't boot from any patch.
            assert!(state.next_boot_patch().is_none());
            assert!(state.last_successfully_booted_patch().is_none());
            Ok(())
        })?;

        Ok(())
    }

    /// When state.json (updater state) is truncated/empty, the updater should
    /// create fresh state with a new client_id rather than crash.
    #[serial]
    #[test]
    fn truncated_state_json_creates_fresh_state() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let original_client_id = with_state(|state| Ok(state.client_id()))?;
        assert!(!original_client_id.is_empty());

        // Truncate state.json to empty.
        let state_path = tmp_dir.path().join("state.json");
        assert!(
            state_path.exists(),
            "state.json should exist before we truncate it"
        );
        std::fs::write(&state_path, "")?;

        // Reinitialize.
        init_for_testing(&tmp_dir, None);

        let new_client_id = with_state(|state| Ok(state.client_id()))?;
        // A fresh state should have a new client_id (the old one was lost
        // along with the truncated file).
        assert!(!new_client_id.is_empty());
        assert_ne!(
            original_client_id, new_client_id,
            "Truncated state.json should generate a new client_id"
        );

        Ok(())
    }

    /// Simulates a crash during boot: currently_booting_patch is set in state
    /// but the patch artifact has been deleted from disk. The updater should
    /// mark it as bad and not try to boot from it.
    #[serial]
    #[test]
    fn crash_during_boot_with_missing_artifact() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        install_fake_patch(1)?;
        // Start booting — sets currently_booting_patch.
        report_launch_start()?;

        with_state(|state| {
            assert_eq!(state.currently_booting_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        // Simulate crash: delete the patch artifact but leave state intact.
        let patches_dir = tmp_dir.path().join("patches");
        assert!(
            patches_dir.exists(),
            "patches directory should exist before we delete it"
        );
        std::fs::remove_dir_all(&patches_dir)?;

        // Reinitialize — simulates process restart after crash.
        // Crash recovery happens during init.
        init_for_testing(&tmp_dir, None);

        with_mut_state(|state| {
            // The patch should be marked as bad (crash recovery detected it).
            assert!(state.is_known_bad_patch(1));
            // And there should be no next boot patch.
            assert!(state.next_boot_patch().is_none());
            Ok(())
        })?;

        Ok(())
    }

    /// After a crash during boot, the updater should queue a PatchInstallFailure
    /// event to report to the server on next update.
    #[serial]
    #[test]
    fn crash_during_boot_queues_failure_event() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        install_fake_patch(1)?;
        report_launch_start()?;
        // Don't call report_launch_success — simulate crash.

        // Reinitialize.
        init_for_testing(&tmp_dir, None);

        with_state(|state| {
            let events = state.copy_events(10);
            assert_eq!(events.len(), 1);
            assert_eq!(
                events[0].identifier,
                crate::events::EventType::PatchInstallFailure
            );
            assert_eq!(events[0].patch_number, 1);
            let message = events[0].message.as_ref().unwrap();
            assert!(
                message.starts_with("crash_recovery: patch 1 failed to boot"),
                "unexpected message: {message}"
            );
            assert!(
                message.contains("elapsed_secs="),
                "message should include elapsed time: {message}"
            );
            assert!(
                message.contains("file_ok="),
                "message should include file status: {message}"
            );
            Ok(())
        })?;

        Ok(())
    }

    /// When a newer patch crashes during boot, it should be marked bad.
    /// The previously-good patch (patch 1) was booted successfully before
    /// patch 2 was installed, so crash recovery should mark only patch 2 bad.
    #[serial]
    #[test]
    fn crash_on_newer_patch_marks_it_bad() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        // Install patch 1, boot it successfully.
        install_fake_patch(1)?;
        report_launch_start()?;
        report_launch_success()?;

        // Install patch 2 (newer), start booting it.
        install_fake_patch(2)?;
        report_launch_start()?;
        // Crash — don't report success.

        // Reinitialize: crash recovery marks patch 2 as bad.
        init_for_testing(&tmp_dir, None);

        with_mut_state(|state| {
            assert!(
                state.is_known_bad_patch(2),
                "Patch 2 should be marked bad after crash"
            );
            assert!(
                !state.is_known_bad_patch(1),
                "Patch 1 should not be marked bad — it booted successfully"
            );
            Ok(())
        })?;

        Ok(())
    }

    /// Verify that load_or_new_on_error preserves client_id even when
    /// patches_state.json is corrupt. The client_id lives in state.json,
    /// not patches_state.json.
    #[serial]
    #[test]
    fn corrupt_patches_state_preserves_client_id() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let original_client_id = with_state(|state| Ok(state.client_id()))?;

        // Corrupt patches_state.json only.
        let patches_state_path = tmp_dir.path().join("patches_state.json");
        assert!(
            patches_state_path.exists(),
            "patches_state.json should exist before we corrupt it"
        );
        std::fs::write(&patches_state_path, "corrupt")?;

        // Reinitialize — state.json is fine, so client_id should survive.
        init_for_testing(&tmp_dir, None);

        let preserved_client_id = with_state(|state| Ok(state.client_id()))?;
        assert_eq!(original_client_id, preserved_client_id);

        Ok(())
    }
}

/// Tests for download validation and error handling during the update flow.
#[cfg(test)]
mod download_validation_tests {
    use anyhow::Result;
    use serial_test::serial;
    use std::path::Path;
    use tempfile::TempDir;

    use crate::{
        network::{testing_set_network_hooks, DownloadResult, PatchCheckResponse},
        test_utils::write_fake_apk,
        updater::tests::init_for_testing,
        with_mut_state, Patch,
    };

    /// When the download hook returns an error, update() should propagate
    /// the error and not leave the updater in a broken state.
    #[serial]
    #[test]
    fn download_failure_propagates_error() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(Patch {
                        number: 1,
                        hash: "abc123".to_string(),
                        download_url: "http://example.com/patch/1".to_string(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, _dest: &Path, _resume_from: u64| {
                anyhow::bail!("Network connection lost");
            },
            |_url, _event| Ok(()),
        );

        let result = crate::update(None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Network connection lost"),);

        // Updater should still be functional — can try again.
        with_mut_state(|state| {
            assert!(state.next_boot_patch().is_none());
            Ok(())
        })?;

        Ok(())
    }

    /// When the download reports 0 bytes written (empty file), the inflate
    /// step should fail and the update should not succeed.
    #[serial]
    #[test]
    fn empty_download_fails_gracefully() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(Patch {
                        number: 1,
                        hash: "abc123".to_string(),
                        download_url: "http://example.com/patch/1".to_string(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| {
                // Write an empty file.
                std::fs::write(dest, b"")?;
                Ok(DownloadResult {
                    total_bytes: 0,
                    content_length: Some(0),
                })
            },
            |_url, _event| Ok(()),
        );

        let result = crate::update(None);
        // Should fail during validation (empty file check), not panic.
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty"),
            "Expected 'empty' in error, got: {err}"
        );

        Ok(())
    }

    /// When the download writes garbage (not zstd), inflate should fail
    /// with a clear error rather than panicking.
    #[serial]
    #[test]
    fn non_zstd_download_fails_with_clear_error() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(Patch {
                        number: 1,
                        hash: "abc123".to_string(),
                        download_url: "http://example.com/patch/1".to_string(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| {
                // Write non-zstd data.
                std::fs::write(dest, b"this is definitely not zstd compressed data!!")?;
                Ok(DownloadResult {
                    total_bytes: 45,
                    content_length: Some(45),
                })
            },
            |_url, _event| Ok(()),
        );

        let result = crate::update(None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("valid zstd magic bytes"),
            "Expected zstd validation error, got: {err}"
        );

        Ok(())
    }

    /// When the server says patch_available=true but patch is None,
    /// update() should return BadServerResponse error.
    #[serial]
    #[test]
    fn patch_available_but_no_patch_object() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: None, // Server says available but doesn't provide patch.
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, _dest: &Path, _resume_from: u64| {
                panic!("Should not attempt download");
            },
            |_url, _event| Ok(()),
        );

        let result = crate::update(None);
        assert!(result.is_err());
        let err = result
            .unwrap_err()
            .downcast::<crate::UpdateError>()
            .unwrap();
        assert_eq!(err, crate::UpdateError::BadServerResponse);

        Ok(())
    }

    /// When the patch check request itself fails, update() should return
    /// an error without touching any state.
    #[serial]
    #[test]
    fn patch_check_network_failure() -> Result<()> {
        use crate::test_utils::install_fake_patch;

        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        install_fake_patch(1)?;

        testing_set_network_hooks(
            |_url, _request| {
                anyhow::bail!("DNS resolution failed");
            },
            |_url, _dest: &Path, _resume_from: u64| {
                panic!("Should not attempt download");
            },
            |_url, _event| Ok(()),
        );

        let result = crate::update(None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("DNS resolution failed"),);

        // Existing patch should still be intact.
        with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        Ok(())
    }
}

/// Tests for roll_back_patches_if_needed, exercising the rollback path directly.
#[cfg(test)]
mod rollback_unit_tests {
    use anyhow::Result;
    use serial_test::serial;
    use tempfile::TempDir;

    use crate::{
        network::{testing_set_network_hooks, PatchCheckResponse, UNEXPECTED_DOWNLOAD},
        report_launch_start, report_launch_success,
        test_utils::install_fake_patch,
        updater::tests::init_for_testing,
        with_mut_state,
    };

    /// Rolling back a patch that was never installed should not error.
    #[serial]
    #[test]
    fn rollback_nonexistent_patch_is_noop() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: false,
                    patch: None,
                    rolled_back_patch_numbers: Some(vec![99]),
                })
            },
            UNEXPECTED_DOWNLOAD,
            |_url, _event| Ok(()),
        );

        // Should not error even though patch 99 was never installed.
        let result = crate::update(None)?;
        assert_eq!(result, crate::UpdateStatus::NoUpdate);

        Ok(())
    }

    // Note: single-patch rollback is already covered by
    // rollback_tests::rolls_back_from_current_patch_to_release.

    /// Rolling back multiple patches at once should clear all of them.
    #[serial]
    #[test]
    fn rollback_multiple_patches() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        // Install patch 1, boot successfully.
        install_fake_patch(1)?;
        report_launch_start()?;
        report_launch_success()?;

        // Install patch 2 on top.
        install_fake_patch(2)?;

        with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(2));
            Ok(())
        })?;

        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: false,
                    patch: None,
                    // Roll back both patches.
                    rolled_back_patch_numbers: Some(vec![1, 2]),
                })
            },
            UNEXPECTED_DOWNLOAD,
            |_url, _event| Ok(()),
        );

        crate::update(None)?;

        with_mut_state(|state| {
            assert!(
                state.next_boot_patch().is_none(),
                "All patches rolled back — should fall back to base"
            );
            Ok(())
        })?;

        Ok(())
    }
}

/// Tests for download resume edge cases — the interaction between sidecar
/// state, partial files, and the server's Range support.
#[cfg(test)]
mod resume_edge_case_tests {
    use anyhow::Result;
    use serial_test::serial;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    use crate::{
        network::{testing_set_network_hooks, DownloadResult, PatchCheckResponse},
        test_utils::write_fake_apk,
        updater::tests::init_for_testing,
        with_mut_state, Patch,
    };

    // The known-good patch bytes: `string_patch "hello world" "hello tests"`.
    const PATCH_BYTES: [u8; 31] = [
        40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0, 0, 0, 0, 5,
        116, 101, 115, 116, 115, 0,
    ];
    const PATCH_HASH: &str = "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45";

    /// Helper: set up network hooks with a patch check returning patch 1,
    /// and a custom download function.
    fn setup_hooks_with_download(download_fn: crate::network::DownloadToPathFn) {
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(Patch {
                        number: 1,
                        hash: PATCH_HASH.to_string(),
                        download_url: "http://example.com/patch/1".to_string(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            download_fn,
            |_url, _event| Ok(()),
        );
    }

    /// When a sidecar exists from a prior attempt but the partial file has
    /// been deleted (e.g. by OS cache cleanup), the updater should start a
    /// fresh download (resume_from=0) rather than sending a bogus Range header.
    #[serial]
    #[test]
    fn sidecar_exists_but_partial_file_deleted() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        // Create sidecar without corresponding partial file.
        let download_dir = tmp_dir.path().join("downloads");
        fs::create_dir_all(&download_dir)?;
        let download_path = download_dir.join("1");
        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://example.com/patch/1".to_string(),
                patch_number: 1,
                expected_size: Some(31),
                expected_hash: PATCH_HASH.to_string(),
            },
        )?;
        // Note: download_path itself does NOT exist — file was deleted.

        setup_hooks_with_download(|_url, dest: &Path, resume_from: u64| {
            // Should start fresh since partial file is missing.
            assert_eq!(
                resume_from, 0,
                "Expected fresh download (resume_from=0) when partial file is missing"
            );
            fs::write(dest, PATCH_BYTES)?;
            Ok(DownloadResult {
                total_bytes: PATCH_BYTES.len() as u64,
                content_length: Some(PATCH_BYTES.len() as u64),
            })
        });

        let result = crate::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        Ok(())
    }

    /// When the server ignores the Range header and returns 200 with the
    /// full file (instead of 206), the download_to_path_default code should
    /// handle this by starting fresh. Here we test the updater still
    /// succeeds end-to-end in this scenario.
    #[serial]
    #[test]
    fn server_ignores_range_header_full_download_succeeds() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        // Pre-create a partial download and sidecar.
        let download_dir = tmp_dir.path().join("downloads");
        fs::create_dir_all(&download_dir)?;
        let download_path = download_dir.join("1");
        fs::write(&download_path, &PATCH_BYTES[..10])?;
        crate::download_state::write_download_state(
            &download_path,
            &crate::download_state::DownloadState {
                url: "http://example.com/patch/1".to_string(),
                patch_number: 1,
                expected_size: None,
                expected_hash: PATCH_HASH.to_string(),
            },
        )?;

        setup_hooks_with_download(|_url, dest: &Path, _resume_from: u64| {
            // Simulate server ignoring Range: write the full file
            // (as download_to_path_default would on a 200 response).
            fs::write(dest, PATCH_BYTES)?;
            Ok(DownloadResult {
                total_bytes: PATCH_BYTES.len() as u64,
                content_length: Some(PATCH_BYTES.len() as u64),
            })
        });

        let result = crate::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        Ok(())
    }

    /// When a download fails mid-transfer, the sidecar should remain so
    /// the next attempt can resume. Verify the sidecar survives the error.
    #[serial]
    #[test]
    fn failed_download_preserves_sidecar_for_retry() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        setup_hooks_with_download(|_url, dest: &Path, _resume_from: u64| {
            // Write partial data then fail.
            fs::write(dest, &PATCH_BYTES[..10])?;
            anyhow::bail!("Connection reset by peer");
        });

        let result = crate::update(None);
        assert!(result.is_err());

        // The sidecar should have been written before the download started.
        let download_path = tmp_dir.path().join("downloads/1");
        let sidecar_path = crate::download_state::sidecar_path(&download_path);
        assert!(
            sidecar_path.exists(),
            "Sidecar should survive a download failure for retry"
        );

        // The partial file should still exist.
        assert!(
            download_path.exists(),
            "Partial download should survive for resume"
        );

        Ok(())
    }

    /// After a failed download, a subsequent update attempt should be able
    /// to resume from the partial file left behind.
    #[serial]
    #[test]
    fn retry_after_failure_resumes_from_partial() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);

        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());

        // Use a const so closures don't capture a local variable.
        const SPLIT_AT: usize = 10;

        // First attempt: write partial data then fail.
        setup_hooks_with_download(|_url, dest: &Path, _resume_from: u64| {
            fs::write(dest, &PATCH_BYTES[..SPLIT_AT])?;
            anyhow::bail!("Connection timeout");
        });
        let _ = crate::update(None); // Expected to fail.

        // Second attempt: should resume from SPLIT_AT.
        setup_hooks_with_download(|_url, dest: &Path, resume_from: u64| {
            assert_eq!(
                resume_from, SPLIT_AT as u64,
                "Expected to resume from byte {SPLIT_AT}"
            );
            // Append remaining bytes.
            use std::io::{Seek, Write};
            let mut file = fs::OpenOptions::new().write(true).open(dest)?;
            file.seek(std::io::SeekFrom::Start(resume_from))?;
            file.write_all(&PATCH_BYTES[SPLIT_AT..])?;
            Ok(DownloadResult {
                total_bytes: PATCH_BYTES.len() as u64,
                content_length: Some(PATCH_BYTES.len() as u64),
            })
        });

        let result = crate::update(None)?;
        assert_eq!(result, crate::UpdateStatus::UpdateInstalled);

        Ok(())
    }
}

#[cfg(test)]
mod multi_engine_tests {
    use anyhow::Result;
    use serial_test::serial;
    use tempfile::TempDir;

    use crate::{
        network::{testing_set_network_hooks, PatchCheckResponse, UNEXPECTED_DOWNLOAD},
        report_launch_start, report_launch_success,
        test_utils::install_fake_patch,
        updater::tests::init_for_testing,
        with_mut_state, with_state,
    };

    /// Sets up no-op network hooks so that the fire-and-forget thread spawned
    /// by `report_launch_success` completes instantly without network I/O.
    /// This prevents leaked threads from interfering with subsequent serial
    /// tests that use mock servers.
    fn set_noop_network_hooks() {
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: false,
                    patch: None,
                    rolled_back_patch_numbers: None,
                })
            },
            UNEXPECTED_DOWNLOAD,
            |_url, _event| Ok(()),
        );
    }

    /// This test demonstrates what would happen if report_launch_start() is called multiple times.
    ///
    /// IMPORTANT: In production, this scenario does NOT occur because:
    /// - The C++ TryLoadFromPatch() in runtime/shorebird/patch_cache.cc uses std::once_flag
    /// - This ensures shorebird_report_launch_start() is only called once per process
    /// - The call happens right before the patched snapshot is actually loaded
    ///
    /// This test documents the Rust API behavior in isolation:
    /// If report_launch_start() IS called multiple times (e.g., from tests or custom embedders):
    ///
    /// Scenario:
    /// 1. Engine A: starts boot (sets currently_booting_patch)
    /// 2. Engine A: completes boot (clears currently_booting_patch)
    /// 3. Engine B: starts boot (RE-SETS currently_booting_patch)
    /// 4. Process is killed before Engine B completes
    /// 5. On restart, crash recovery sees currently_booting_patch set and marks patch as bad
    ///
    /// This would be a FALSE POSITIVE - the patch didn't actually fail.
    #[serial]
    #[test]
    fn multi_engine_false_positive_rollback() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);
        set_noop_network_hooks();

        // Install a patch
        install_fake_patch(1)?;

        // Verify patch is installed and ready to boot
        with_mut_state(|state| {
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
            assert!(state.currently_booting_patch().is_none());
            assert!(!state.is_known_bad_patch(1));
            Ok(())
        })?;

        // ========================================
        // Engine A: Full successful boot cycle
        // ========================================
        report_launch_start()?;

        with_state(|state| {
            // currently_booting_patch should be set
            assert_eq!(state.currently_booting_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        report_launch_success()?;

        with_state(|state| {
            // After success, currently_booting_patch should be cleared
            assert!(state.currently_booting_patch().is_none());
            // And last_successfully_booted_patch should be set
            assert_eq!(
                state.last_successfully_booted_patch().map(|p| p.number),
                Some(1)
            );
            Ok(())
        })?;

        // ========================================
        // Engine B: Starts boot but process is killed
        // ========================================
        // In real scenario, this is another FlutterEngine in the same process
        // calling report_launch_start() after Engine A already finished.
        report_launch_start()?;

        with_state(|state| {
            // BUG: currently_booting_patch is now set AGAIN!
            // This is the root cause - Engine B re-set the flag.
            assert_eq!(state.currently_booting_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        // Process is killed here (Engine B never calls report_launch_success)
        // We simulate this by reinitializing without completing Engine B's boot.

        // ========================================
        // New process: Crash recovery kicks in
        // ========================================
        init_for_testing(&tmp_dir, None);
        set_noop_network_hooks();

        // The patch should NOT be marked as bad (Engine A booted successfully!)
        // But when report_launch_start() is called multiple times at the Rust level,
        // the second call re-sets currently_booting_patch, causing a false positive.
        //
        // NOTE: The C++ fix in TryLoadFromPatch() uses std::once_flag to prevent
        // multiple calls, so this scenario cannot happen in production. This test
        // documents the Rust API behavior when called directly without that guard.
        with_mut_state(|state| {
            // When report_launch_start() is called multiple times without a guard,
            // the patch is incorrectly marked as known bad.
            assert!(
                state.is_known_bad_patch(1),
                "Expected patch to be marked bad when report_launch_start() called twice"
            );

            // The next_boot_patch is None because patch was marked as bad
            assert!(
                state.next_boot_patch().is_none(),
                "Expected next_boot_patch to be None after patch marked bad"
            );

            Ok(())
        })?;

        Ok(())
    }

    /// Test that interleaved start/success calls are handled correctly when
    /// successes come after all starts.
    ///
    /// Scenario: start_A, start_B, success_A, [crash]
    /// This works correctly because success_A clears the flag before the crash.
    ///
    /// NOTE: In production, the C++ std::once_flag in TryLoadFromPatch() prevents
    /// multiple report_launch_start() calls. This test documents Rust API behavior.
    #[serial]
    #[test]
    fn interleaved_boot_calls_success_clears_flag() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        init_for_testing(&tmp_dir, None);
        set_noop_network_hooks();

        install_fake_patch(1)?;

        // start_A
        report_launch_start()?;
        // start_B (simulated - same call, re-sets the same flag)
        report_launch_start()?;

        with_state(|state| {
            assert_eq!(state.currently_booting_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        // success_A
        report_launch_success()?;

        with_state(|state| {
            // Flag is cleared by success_A
            assert!(state.currently_booting_patch().is_none());
            Ok(())
        })?;

        // Process killed here - success_B never called
        init_for_testing(&tmp_dir, None);
        set_noop_network_hooks();

        with_mut_state(|state| {
            // This should work correctly because success_A already cleared the flag
            assert!(!state.is_known_bad_patch(1));
            assert_eq!(state.next_boot_patch().map(|p| p.number), Some(1));
            Ok(())
        })?;

        Ok(())
    }
}
