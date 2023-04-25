// This file's job is to be the Rust API for the updater.

use std::fmt::{Display, Formatter};

use crate::cache::{PatchInfo, UpdaterState};
use crate::config::{set_config, with_config, ResolvedConfig};
use crate::logging::init_logging;
use crate::network::{download_to_path, send_patch_check_request};
use crate::yaml::YamlConfig;
use std::fs;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};

#[cfg(test)]
// Expose testing_reset_config for integration tests.
pub use crate::config::testing_reset_config;
#[cfg(test)]
pub use crate::network::{
    testing_set_network_hooks, DownloadFileFn, Patch, PatchCheckRequest, PatchCheckRequestFn,
};

use anyhow::Context;

pub enum UpdateStatus {
    NoUpdate,
    UpdateAvailable,
    UpdateDownloaded,
    UpdateInstalled,
    UpdateHadError,
}

impl Display for UpdateStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateStatus::NoUpdate => write!(f, "No update"),
            UpdateStatus::UpdateAvailable => write!(f, "Update available"),
            UpdateStatus::UpdateDownloaded => write!(f, "Update downloaded"),
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

/// Initialize the updater library.
/// Takes a AppConfig struct and a yaml string.
/// The yaml string is the contents of the shorebird.yaml file.
/// The AppConfig struct is information about the running app and where
/// the updater should keep its cache.
pub fn init(app_config: AppConfig, yaml: &str) -> Result<(), UpdateError> {
    init_logging();
    let config = YamlConfig::from_yaml(&yaml)
        .map_err(|err| UpdateError::InvalidArgument("yaml".to_string(), err.to_string()))?;
    set_config(app_config, config).map_err(|err| UpdateError::InvalidState(err.to_string()))
}

fn check_for_update_internal(config: &ResolvedConfig) -> bool {
    // Load UpdaterState from disk
    // If there is no state, make an empty state.
    let state = UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
    // Send info from app + current slot to server.
    let response_result = send_patch_check_request(&config, &state);
    match response_result {
        Err(err) => {
            error!("Failed update check: {err}");
            return false;
        }
        Ok(response) => {
            return response.patch_available;
        }
    }
}

/// Synchronously checks for an update and returns true if an update is available.
pub fn check_for_update() -> bool {
    return with_config(check_for_update_internal);
}

fn check_hash(path: &Path, expected_string: &str) -> anyhow::Result<bool> {
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
    if !hash_matches {
        warn!(
            "Hash mismatch: {:?}, expected: {}, got: {:?}",
            path,
            expected_string,
            hex::encode(hash)
        );
    } else {
        info!("Hash match: {:?}", path);
    }
    return Ok(hash_matches);
}

fn app_data_dir_from_libapp_path(libapp_path: &str) -> anyhow::Result<PathBuf> {
    // "/data/app/~~7LtReIkm5snW_oXeDoJ5TQ==/com.example.shorebird_test-rpkDZSLBRv2jWcc1gQpwdg==/lib/x86_64/libapp.so"
    let path = PathBuf::from(libapp_path);
    let root = path.ancestors().nth(3).context("Invalid libapp path")?;
    Ok(PathBuf::from(root))
}

struct ArchNames {
    apk_split: &'static str,
    lib_dir: &'static str,
}

fn android_arch_names() -> &'static ArchNames {
    // This was generated by looking at what apk splits are generated by
    // bundletool.
    // https://developer.android.com/ndk/guides/abis
    #[cfg(target_arch = "x86")]
    static ARCH: ArchNames = ArchNames {
        apk_split: "x86",
        lib_dir: "x86",
    };
    #[cfg(target_arch = "x86_64")]
    // x86_64 uses _ for both split and library paths.
    static ARCH: ArchNames = ArchNames {
        apk_split: "x86_64", // e.g. standalone-x86_64_hdpi.apk
        lib_dir: "x86_64",   // e.g. lib/x86_64/libapp.so
    };
    #[cfg(target_arch = "aarch64")]
    // Note the _ in the split name, but the - in the lib dir.
    static ARCH: ArchNames = ArchNames {
        apk_split: "arm64_v8a",
        lib_dir: "arm64-v8a",
    };
    #[cfg(target_arch = "arm")]
    // Note the _ in the split name, but the - in the lib dir.
    static ARCH: ArchNames = ArchNames {
        apk_split: "armeabi_v7a", // e.g. base-armeabi_v7a.apk
        lib_dir: "armeabi-v7a",   // e.g. lib/armeabi-v7a/libapp.so
    };
    return &ARCH;
}

fn get_relative_lib_path(lib_name: &str) -> PathBuf {
    PathBuf::from("lib")
        .join(android_arch_names().lib_dir)
        .join(lib_name)
}

// This is just a tuple of the archive and the internal path to the library.
// Ideally we'd just return the ZipFile itself, but I don't know how to set
// up the references correctly, ZipFile contains a borrow into the ZipArchive.
// And I'm not the right Rust to keep a reference to both with proper lifetimes.
struct ZipLocation {
    archive: zip::ZipArchive<fs::File>,
    internal_path: String,
}

fn check_for_lib_path(zip_path: &Path, lib_path: &str) -> anyhow::Result<ZipLocation> {
    let apk = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    if apk.file_names().any(|name| name == lib_path) {
        return Ok(ZipLocation {
            archive: apk,
            internal_path: lib_path.to_owned(),
        });
    }
    return Err(anyhow::anyhow!("Library not found in APK"));
}

/// Given a directory of APKs, find the one that contains the library we want.
/// This has to be done due to split APKs.
fn find_and_open_lib(apks_dir: &Path, lib_name: &str) -> anyhow::Result<ZipLocation> {
    // Read the library out of the APK.  We only really need to do this if it
    // isn't already extracted on disk (which it won't be by default from the
    // play store).

    // First check ones with our arch in the name, in any order.
    let arch = android_arch_names();
    let lib_path = get_relative_lib_path(lib_name)
        .to_str()
        .context("Invalid lib path")?
        .to_owned();

    for entry in fs::read_dir(apks_dir)? {
        let entry = entry?;
        let path = entry.path(); // returns the absolute path.
        if path.is_dir() {
            continue;
        }
        // Using match to avoid unwrap possibly panicking.
        match path.file_name() {
            None => continue,
            Some(filename) => match filename.to_str() {
                None => continue,
                Some(filename) => {
                    if !filename.ends_with(".apk") {
                        continue;
                    }
                    if !filename.contains(arch.apk_split) {
                        debug!("Ignoring APK: {:?}", path);
                        continue;
                    }
                }
            },
        }
        debug!("Checking APK split: {:?}", path);
        if let Ok(zip) = check_for_lib_path(&path, &lib_path) {
            debug!("Found lib in apk split: {:?}", path);
            return Ok(zip);
        }
    }
    let base_apk_path = apks_dir.join("base.apk");
    debug!("Checking base APK: {:?}", base_apk_path);
    return check_for_lib_path(&base_apk_path, &lib_path);
}

fn open_base_lib(apks_dir: &Path, lib_name: &str) -> anyhow::Result<Cursor<Vec<u8>>> {
    // As far as I can tell, Android provides no apis for reading per-platform
    // assets (e.g. libapp.so) from an APK.  Both Facebook and Chromium
    // seem to have written their own code to do this:
    // https://github.com/facebook/SoLoader/blob/main/java/com/facebook/soloader/DirectApkSoSource.java
    // https://chromium.googlesource.com/chromium/src/base/+/a5ca5def0453df367b9c42e9817a33d2a21e75fe/android/java/src/org/chromium/base/library_loader/Linker.java
    // Previously I tried reading libapp.so from from the AssetManager, but
    // it does show the lib/ directory in the list of assets.
    // https://github.com/shorebirdtech/updater/pull/6

    // Ideally we would do this apk reading from the C++ side and keep the rust
    // portable, but we have a zip library here, and don't on the C++ side.

    let mut zip_location = find_and_open_lib(apks_dir, lib_name)?;
    let mut zip_file = zip_location
        .archive
        .by_name(&zip_location.internal_path)
        .context("Failed to find libapp.so in APK")?;

    // Cursor (rather than ZipFile) is only necessary because bipatch expects
    // Seek + Read for the input file.  I don't think it actually needs to
    // seek backwards, so Read is probably sufficient.  If we made bipatch
    // only depend on Read we could avoid loading the library fully into memory.
    let mut buffer = Vec::new();
    zip_file.read_to_end(&mut buffer)?;
    Ok(Cursor::new(buffer))
}

// Run the update logic with the resolved config.
fn update_internal(config: &ResolvedConfig) -> anyhow::Result<UpdateStatus> {
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
    download_to_path(&patch.download_url, &download_path)?;

    // FIXME: This makes the assumption that the last path provided is the full
    // path to the libapp.so file.  This is true for the current engine, but
    // may not be true in the future.  Better would be for the engine to
    // pass us the path to the base.apk.
    // https://github.com/shorebirdtech/shorebird/issues/283
    // This is where the paths are set today:
    // First path is "libapp.so" (for dlopen), second is a full path:
    // https://github.com/flutter/engine/blob/a7c9cc58a71c5850be0215ab1997db92cc5e8d3e/shell/platform/android/io/flutter/embedding/engine/loader/FlutterLoader.java#L264
    // Which is composed from nativeLibraryDir:
    // https://developer.android.com/reference/android/content/pm/ApplicationInfo#nativeLibraryDir
    let full_libapp_path = config
        .original_libapp_paths
        .last()
        .context("No libapp paths")?;
    // We could probably use sourceDir instead?
    // https://developer.android.com/reference/android/content/pm/ApplicationInfo#sourceDir
    // and splitSourceDirs (api 21+)
    // https://developer.android.com/reference/android/content/pm/ApplicationInfo#splitSourceDirs
    debug!("Finding apk from: {:?}", full_libapp_path);
    let app_dir = app_data_dir_from_libapp_path(full_libapp_path)?;
    debug!("app_dir: {:?}", app_dir);
    let base_r = open_base_lib(&app_dir, "libapp.so")?;

    let output_path = download_dir.join(format!("{}.full", patch.number.to_string()));
    inflate(&download_path, base_r, &output_path)?;

    // Check the hash before moving into place.
    let hash_ok = check_hash(&output_path, &patch.hash)?;
    if !hash_ok {
        return Err(UpdateError::InvalidState("Hash mismatch.  This is most often caused by using the same version number with a different app binary.".to_string()).into());
    }

    let patch_info = PatchInfo {
        path: output_path
            .to_str()
            .context("invalid output path")?
            .to_string(),
        number: patch.number,
    };
    // Move/state update should be "atomic" (it isn't today).
    state.install_patch(patch_info)?;
    info!("Patch {} successfully installed.", patch.number);
    // Should set some state to say the status is "update required" and that
    // we now have a different "next" version of the app from the current
    // booted version (patched or not).
    return Ok(UpdateStatus::UpdateInstalled);
}

/// Given a path to a patch file, and a base file, apply the patch to the base
/// and write the result to the output path.
fn inflate<RS>(patch_path: &Path, base_r: RS, output_path: &Path) -> anyhow::Result<()>
where
    RS: Read + Seek,
{
    use comde::de::Decompressor;
    use comde::zstd::ZstdDecompressor;
    info!("Patch is compressed, inflating...");
    use std::io::{BufReader, BufWriter};

    // Open all our files first for error clarity.  Otherwise we might see
    // PipeReader/Writer errors instead of file open errors.
    info!("Reading patch file: {:?}", patch_path);
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
pub fn next_boot_patch() -> Option<PatchInfo> {
    return with_config(|config| {
        let state = UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
        return state.next_boot_patch();
    });
}

/// The patch which is currently booted.  This is None until
/// report_launch_start() is called at which point it is copied from
/// next_boot_patch.
pub fn current_boot_patch() -> Option<PatchInfo> {
    return with_config(|config| {
        let state = UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
        return state.current_boot_patch();
    });
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
pub fn report_launch_failure() -> Result<(), UpdateError> {
    info!("Reporting failed launch.");
    with_config(|config| {
        let mut state =
            UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);

        let patch = state
            .current_boot_patch()
            .ok_or(UpdateError::InvalidState("No current patch".to_string()))?;
        state.mark_patch_as_bad(&patch);
        state.activate_latest_bootable_patch()
    })
}

pub fn report_launch_success() -> Result<(), UpdateError> {
    with_config(|config| {
        let mut state =
            UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);

        let patch = state
            .current_boot_patch()
            .ok_or(UpdateError::InvalidState("No current patch".to_string()))?;
        state.mark_patch_as_good(&patch);
        state.save().map_err(|_| UpdateError::FailedToSaveState)
    })
}

/// Synchronously checks for an update and downloads and installs it if available.
pub fn update() -> UpdateStatus {
    return with_config(|config| {
        let result = update_internal(&config);
        match result {
            Err(err) => {
                error!("Problem updating: {err}");
                error!("{}", err.backtrace());
                return UpdateStatus::UpdateHadError;
            }
            Ok(status) => status,
        }
    });
}

/// This does not return status.  The only output is the change to the saved
/// cache. The Engine calls this during boot and it will check for an update
/// and install it if available.
pub fn start_update_thread() {
    // This holds the lock on the config for the entire duration of the update
    // call which is wrong. We should be able to release the lock during the
    // network requests.
    std::thread::spawn(move || {
        let status = update();
        info!("Update thread finished with status: {}", status);
    });
}

#[cfg(test)]
mod tests {
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
                original_libapp_paths: vec!["original_libapp_path".to_string()],
            },
            "app_id: 1234",
        )
        .unwrap();
    }

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
            info!("artifact_path: {:?}", artifact_path);
            fs::create_dir_all(&download_dir).unwrap();
            fs::write(&artifact_path, "hello").unwrap();

            let mut state =
                UpdaterState::load_or_new_on_error(&config.cache_dir, &config.release_version);
            state
                .install_patch(PatchInfo {
                    path: artifact_path.to_str().unwrap().to_string(),
                    number: 1,
                })
                .expect("move failed");
            state.save().expect("save failed");
        });
        assert!(crate::next_boot_patch().is_some());
        // pretend we booted from it
        crate::report_launch_start().unwrap();
        crate::report_launch_success().unwrap();
        assert!(crate::next_boot_patch().is_some());
        // mark it bad.
        crate::report_launch_failure().unwrap();
        // Technically might need to "reload"
        // ask for current patch (should get none).
        assert!(crate::next_boot_patch().is_none());
    }

    #[test]
    fn hash_matches() {
        let tmp_dir = TempDir::new("example").unwrap();

        let input_path = tmp_dir.path().join("input");
        fs::write(&input_path, "hello world").unwrap();

        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(super::check_hash(&input_path, expected).unwrap());

        // modify hash to not match
        let expected = "a94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert_eq!(super::check_hash(&input_path, expected).unwrap(), false);

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

    #[test]
    fn report_launch_result_with_no_current_patch() {
        let tmp_dir = TempDir::new("example").unwrap();
        init_for_testing(&tmp_dir);
        assert_eq!(
            crate::report_launch_failure(),
            Err(crate::UpdateError::InvalidState(
                "No current patch".to_string()
            ))
        );
        assert_eq!(
            crate::report_launch_success(),
            Err(crate::UpdateError::InvalidState(
                "No current patch".to_string()
            ))
        );
    }
}
