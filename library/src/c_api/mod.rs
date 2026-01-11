// This file handles translating the updater library's types into C types.

// Currently manually prefixing all functions with "shorebird_" to avoid
// name collisions with other libraries.
// `cbindgen:prefix-with-name` could do this for us.

/// This file contains the C API for the updater library.
/// It is intended to be used by language bindings, and is not intended to be
/// used directly by Rust code.
/// The C API is not stable and may change at any time.
/// You can see usage of this API in Shorebird's Flutter engine:
/// <https://github.com/shorebirdtech/engine/blob/shorebird/dev/shell/common/shorebird.cc>
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;

use crate::{updater, UpdateStatus};

use self::c_file::CFileProvider;

mod c_file;

/// Struct containing configuration parameters for the updater.
/// Passed to all updater functions.
/// NOTE: If this struct is changed all language bindings must be updated.
#[repr(C)]
pub struct AppParameters {
    /// release_version, required.  Named version of the app, off of which
    /// updates are based.  Can be either a version number or a hash.
    pub release_version: *const libc::c_char,

    /// Array of paths to the original aot library, required.  For Flutter apps
    /// these are the paths to the bundled libapp.so.  May be used for
    /// compression downloaded artifacts.
    pub original_libapp_paths: *const *const libc::c_char,

    /// Length of the original_libapp_paths array.
    pub original_libapp_paths_size: libc::c_int,

    /// Path to app storage directory where the updater will store serialized
    /// state and other data that persists between releases.
    pub app_storage_dir: *const libc::c_char,

    /// Path to cache directory where the updater will store downloaded
    /// artifacts and data that can be deleted when a new release is detected.
    pub code_cache_dir: *const libc::c_char,
}

/// An unknown error occurred while updating. The update was not installed.
/// This is a catch-all for errors that don't fit into the other categories.
pub const SHOREBIRD_UPDATE_ERROR: i32 = -1;

/// No update is available (e.g. the app is already up-to-date)
pub const SHOREBIRD_NO_UPDATE: i32 = 0;

/// An update was installed successfully. It will boot from the update on the
/// next app launch.
pub const SHOREBIRD_UPDATE_INSTALLED: i32 = 1;

/// An error occurred while updating. The update was not installed.
pub const SHOREBIRD_UPDATE_HAD_ERROR: i32 = 2;

/// The downloaded patch was not installed because it was invalid.
pub const SHOREBIRD_UPDATE_IS_BAD_PATCH: i32 = 3;

#[repr(C)]
pub struct UpdateResult {
    pub status: i32,
    pub message: *const libc::c_char,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FileCallbacks {
    /// Opens the "file" (actually an in-memory buffer) and returns a handle.
    pub open: extern "C" fn() -> *mut libc::c_void,

    /// Reads count bytes from the file into buffer.  Returns the number of
    /// bytes read.
    pub read: extern "C" fn(file_handle: *mut libc::c_void, buffer: *mut u8, count: usize) -> usize,

    /// Moves the file pointer to the given offset relative from whence (one of
    /// libc::SEEK_SET, libc::SEEK_CUR, or libc::SEEK_END). Returns the new
    /// offset relative to the start of the file.
    pub seek: extern "C" fn(file_handle: *mut libc::c_void, offset: i64, whence: i32) -> i64,

    /// Closes and frees the file handle.
    pub close: extern "C" fn(file_handle: *mut libc::c_void),
}

/// Converts a C string to a Rust string, does not free the C string.
fn to_rust(c_string: *const libc::c_char) -> anyhow::Result<String> {
    anyhow::ensure!(!c_string.is_null(), "Null string passed to to_rust");
    let c_str = unsafe { CStr::from_ptr(c_string) };
    Ok(c_str.to_str()?.to_string())
}

fn to_rust_option(c_string: *const c_char) -> anyhow::Result<Option<String>> {
    if c_string.is_null() {
        return Ok(None);
    }
    Ok(Some(to_rust(c_string)?))
}

/// Converts a Rust string to a C string, caller must free the C string.
fn allocate_c_string(rust_string: &str) -> anyhow::Result<*mut c_char> {
    let c_str = CString::new(rust_string)?;
    Ok(c_str.into_raw())
}

fn to_rust_vector(
    c_array: *const *const libc::c_char,
    size: libc::c_int,
) -> anyhow::Result<Vec<String>> {
    let mut result = Vec::new();
    for i in 0..size {
        let c_string = unsafe { *c_array.offset(i as isize) };
        result.push(to_rust(c_string)?);
    }
    Ok(result)
}

fn app_config_from_c(c_params: *const AppParameters) -> anyhow::Result<updater::AppConfig> {
    anyhow::ensure!(
        !c_params.is_null(),
        "Null parameters passed to app_config_from_c"
    );
    let c_params_ref = unsafe { &*c_params };

    Ok(updater::AppConfig {
        app_storage_dir: to_rust(c_params_ref.app_storage_dir)?,
        code_cache_dir: to_rust(c_params_ref.code_cache_dir)?,
        release_version: to_rust(c_params_ref.release_version)?,
        original_libapp_paths: to_rust_vector(
            c_params_ref.original_libapp_paths,
            c_params_ref.original_libapp_paths_size,
        )?,
    })
}

/// Helper function to log errors instead of panicking or returning a result.
fn log_on_error<F, R>(f: F, context: &str, error_result: R) -> R
where
    F: FnOnce() -> Result<R, anyhow::Error>,
{
    f().unwrap_or_else(|e| {
        shorebird_error!("Error {}: {:?}", context, e);
        error_result
    })
}

/// Configures updater.  First parameter is a struct containing configuration
/// from the running app.  Second parameter is a YAML string containing
/// configuration compiled into the app.  Returns true on success and false on
/// failure. If false is returned, the updater library will not be usable.
#[no_mangle]
pub extern "C" fn shorebird_init(
    c_params: *const AppParameters,
    c_file_callbacks: FileCallbacks,
    c_yaml: *const libc::c_char,
) -> bool {
    log_on_error(
        || {
            let config = app_config_from_c(c_params)?;
            let file_provider = Box::new(CFileProvider {
                file_callbacks: c_file_callbacks,
            });
            let yaml_string = to_rust(c_yaml)?;
            updater::init(config, file_provider, &yaml_string)?;
            Ok(true)
        },
        "initializing updater",
        false,
    )
}

/// Returns if the app should run the updater automatically on launch.
#[no_mangle]
pub extern "C" fn shorebird_should_auto_update() -> bool {
    log_on_error(
        updater::should_auto_update,
        "fetching update behavior",
        true,
    )
}

/// The currently running patch number, or 0 if the release has not been
/// patched.
#[no_mangle]
pub extern "C" fn shorebird_current_boot_patch_number() -> usize {
    log_on_error(
        || Ok(updater::current_boot_patch()?.map_or(0, |p| p.number)),
        "fetching next_boot_patch_number",
        0,
    )
}

/// The patch number that will boot on the next run of the app, or 0 if there is
/// no next patch.
#[no_mangle]
pub extern "C" fn shorebird_next_boot_patch_number() -> usize {
    log_on_error(
        || Ok(updater::next_boot_patch()?.map_or(0, |p| p.number)),
        "fetching next_boot_patch_number",
        0,
    )
}

fn path_to_c_string(path: Option<PathBuf>) -> anyhow::Result<*mut c_char> {
    Ok(match path {
        Some(v) => allocate_c_string(v.to_str().unwrap())?,
        None => std::ptr::null_mut(),
    })
}

fn to_update_result(status: anyhow::Result<UpdateStatus>) -> UpdateResult {
    let result = match status {
        Ok(status) => {
            let message = status.to_string();
            return UpdateResult {
                status: status as i32,
                message: allocate_c_string(message.as_str())
                    .unwrap_or_else(|_| std::ptr::null_mut()),
            };
        }
        Err(err) => UpdateResult {
            status: SHOREBIRD_UPDATE_ERROR,
            message: allocate_c_string(&err.to_string()).unwrap_or_else(|_| std::ptr::null_mut()),
        },
    };
    return result;
}

/// Performs integrity checks on the next boot patch. If the patch fails these checks, the patch
/// will be deleted and the next boot patch will be set to the last successfully booted patch or
/// the base release if there is no last successfully booted patch.
#[no_mangle]
pub extern "C" fn shorebird_validate_next_boot_patch() {
    log_on_error(
        || updater::validate_next_boot_patch(),
        "validating next_boot_patch",
        (),
    );
}

/// The path to the patch that will boot on the next run of the app, or NULL if
/// there is no next patch.
#[no_mangle]
pub extern "C" fn shorebird_next_boot_patch_path() -> *mut c_char {
    log_on_error(
        || {
            let maybe_path = updater::next_boot_patch()?.map(|p| p.path);
            path_to_c_string(maybe_path)
        },
        "fetching next_boot_patch_path",
        std::ptr::null_mut(),
    )
}

/// Free a string returned by the updater library.
/// # Safety
///
/// If this function is called with a non-null pointer, it must be a pointer
/// returned by the updater library.
#[no_mangle]
pub unsafe extern "C" fn shorebird_free_string(c_string: *const c_char) {
    if c_string.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(c_string as *mut c_char));
    }
}

#[no_mangle]
pub unsafe extern "C" fn shorebird_free_update_result(result: *mut UpdateResult) {
    if result.is_null() {
        return;
    }
    let message = (*result).message;
    if !message.is_null() {
        shorebird_free_string(message);
    }
    unsafe {
        drop(Box::from_raw(result));
    }
}

/// Check for an update.  Returns true if an update is available.
#[no_mangle]
pub extern "C" fn shorebird_check_for_update() -> bool {
    log_on_error(
        || updater::check_for_downloadable_update(None),
        "checking for update",
        false,
    )
}

/// Check for an update on the first non-null channel of:
///   1. `c_channel`
///   2. The channel specified in shorebird.yaml
///   3. The default "stable" channel
///
/// Returns true if an update exists that has not yet been downloaded.
#[no_mangle]
pub extern "C" fn shorebird_check_for_downloadable_update(c_channel: *const c_char) -> bool {
    log_on_error(
        || {
            let channel = to_rust_option(c_channel)?;
            updater::check_for_downloadable_update(channel.as_deref())
        },
        "checking for update",
        false,
    )
}

/// Synchronously download an update if one is available.
#[no_mangle]
pub extern "C" fn shorebird_update() {
    log_on_error(
        || updater::update(None).map(|result| shorebird_info!("Update result: {}", result)),
        "downloading update",
        (),
    );
}

/// Synchronously download an update on the first non-null channel of:
///   1. `c_channel`
///   2. The channel specified in shorebird.yaml
///   3. The default "stable" channel
///
/// Returns an [UpdateResult] indicating whether the update was successful.
#[no_mangle]
pub extern "C" fn shorebird_update_with_result(c_channel: *const c_char) -> *const UpdateResult {
    let channel = to_rust_option(c_channel);
    let result = match channel {
        Ok(channel) => to_update_result(updater::update(channel.as_deref())),
        Err(err) => to_update_result(Err(err)),
    };
    return Box::into_raw(Box::new(result));
}

/// Start a thread to download an update if one is available.
#[no_mangle]
pub extern "C" fn shorebird_start_update_thread() {
    updater::start_update_thread();
}

/// Tell the updater that we're launching from what it told us was the
/// next patch to boot from. This will copy the next boot patch to be the
/// `current_boot` patch.
///
/// It is required to call this function before calling
/// `shorebird_report_launch_success` or `shorebird_report_launch_failure`.
#[no_mangle]
pub extern "C" fn shorebird_report_launch_start() {
    log_on_error(updater::report_launch_start, "reporting launch start", ());
}

/// Report that the app failed to launch.  This will cause the updater to
/// attempt to roll back to the previous version if this version has not
/// been launched successfully before.
#[no_mangle]
pub extern "C" fn shorebird_report_launch_failure() {
    log_on_error(
        updater::report_launch_failure,
        "reporting launch failure",
        (),
    );
}

/// Report that the app launched successfully.  This will mark the current
/// as having been launched successfully.  We don't currently do anything
/// with this information, but it could be used to record a point at which
/// we will not roll back from.
///
/// This is not currently wired up to be called from the Engine.  It's unclear
/// where best to connect it.  Expo waits 5 seconds after the app launches
/// and then marks the launch as successful.  We could do something similar.
#[no_mangle]
pub extern "C" fn shorebird_report_launch_success() {
    log_on_error(
        updater::report_launch_success,
        "reporting launch success",
        (),
    );
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        network::{testing_set_network_hooks, PatchCheckResponse},
        test_utils::write_fake_apk,
    };
    use anyhow::Ok;
    use serial_test::serial;
    use tempdir::TempDir;
    use updater::testing_reset_config;

    use std::{ffi::CString, ptr::null_mut};

    fn c_string(string: &str) -> *mut libc::c_char {
        CString::new(string).unwrap().into_raw()
    }

    fn free_c_string(string: *mut libc::c_char) {
        unsafe {
            drop(CString::from_raw(string));
        }
    }

    fn c_array(strings: Vec<String>) -> *mut *mut libc::c_char {
        let mut c_strings = Vec::new();
        for string in strings {
            c_strings.push(c_string(&string));
        }
        // Make sure we're not wasting space.
        c_strings.shrink_to_fit();
        assert!(c_strings.len() == c_strings.capacity());

        let ptr = c_strings.as_mut_ptr();
        std::mem::forget(c_strings);
        ptr
    }

    fn free_c_array(strings: *mut *mut libc::c_char, size: usize) {
        let v = unsafe { Vec::from_raw_parts(strings, size, size) };

        // Now drop one string at a time.
        for string in v {
            free_c_string(string);
        }
    }

    // libapp_path is currently Android-style with a virtual path
    // of at least 3 directories in depth ending in libapp.so.
    fn parameters(tmp_dir: &TempDir, libapp_path: &str) -> super::AppParameters {
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
        let app_paths_vec = vec![libapp_path.to_owned()];
        let app_paths_size = app_paths_vec.len() as i32;
        let app_paths = c_array(app_paths_vec);

        super::AppParameters {
            app_storage_dir: c_string(&cache_dir),
            code_cache_dir: c_string(&cache_dir),
            release_version: c_string("1.0.0"),
            original_libapp_paths: app_paths as *const *const libc::c_char,
            original_libapp_paths_size: app_paths_size,
        }
    }

    fn free_parameters(params: super::AppParameters) {
        free_c_string(params.app_storage_dir as *mut libc::c_char);
        free_c_string(params.code_cache_dir as *mut libc::c_char);
        free_c_string(params.release_version as *mut libc::c_char);
        free_c_array(
            params.original_libapp_paths as *mut *mut libc::c_char,
            params.original_libapp_paths_size as usize,
        )
    }

    #[serial]
    #[test]
    fn init_with_nulls() {
        testing_reset_config();
        // Should log but not crash.
        assert!(!shorebird_init(
            std::ptr::null(),
            FileCallbacks::new(),
            std::ptr::null()
        ));

        // free_string also doesn't crash with null.
        unsafe { shorebird_free_string(std::ptr::null_mut()) }
        // free_update_result also doesn't crash with null.
        unsafe { shorebird_free_update_result(std::ptr::null_mut()) }
    }

    #[serial]
    #[test]
    fn init_with_null_app_parameters() {
        testing_reset_config();
        // Should log but not crash.
        let c_params = AppParameters {
            app_storage_dir: std::ptr::null(),
            code_cache_dir: std::ptr::null(),
            release_version: std::ptr::null(),
            original_libapp_paths: std::ptr::null(),
            original_libapp_paths_size: 0,
        };
        assert!(!shorebird_init(
            &c_params,
            FileCallbacks::new(),
            std::ptr::null()
        ));
    }

    #[serial]
    #[test]
    fn init_with_bad_yaml() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();
        let c_params = parameters(&tmp_dir, "/dir/lib/arm64/libapp.so");
        let c_yaml = c_string("bad yaml");
        assert!(!shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);
    }

    #[serial]
    #[test]
    fn init_with_invalid_patch_verification() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();
        let c_params = parameters(&tmp_dir, "/dir/lib/arm64/libapp.so");
        let c_yaml = c_string("app_id: foo\npatch_verification: bogus_mode");
        // Invalid patch_verification causes init to fail and return false
        assert!(!shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);
    }

    #[serial]
    #[test]
    fn yaml_parsing() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();
        let c_params = parameters(&tmp_dir, "/dir/lib/arm64/libapp.so");
        let c_yaml = c_string(
            "
        app_id: foo
        channel: bar
        base_url: baz
        auto_update: false",
        );
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);
        assert!(!shorebird_should_auto_update());
    }

    #[serial]
    #[test]
    fn empty_state_no_update() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();
        let c_params = parameters(&tmp_dir, "/dir/lib/arm64/libapp.so");
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // Number is 0 and path is empty (but do not crash) when we have an
        // empty cache and update has not been called.
        assert_eq!(shorebird_current_boot_patch_number(), 0);
        assert_eq!(shorebird_next_boot_patch_number(), 0);
        assert_eq!(shorebird_next_boot_patch_path(), null_mut());

        // Similarly we can report launches with no patch without crashing.
        shorebird_report_launch_start();
        shorebird_report_launch_success();
        shorebird_report_launch_failure();
    }

    #[serial]
    #[test]
    fn patch_success() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        // Generated by `string_patch "hello world" "hello tests"`
        let base = "hello world";
        let expected_new: &str = "hello tests";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // set up the network hooks to return a patch.
        testing_set_network_hooks(
            |_url, request| {
                // We didn't specify a channel in either the shorebird_check_for_downloadable_update
                // call or the shorebird.yaml, so we should default to "stable".
                assert_eq!(request.channel, "stable");
                // Generated by `string_patch "hello world" "hello tests"`
                let hash = "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45";
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url| {
                // Generated by `string_patch "hello world" "hello tests"`
                let patch_bytes: Vec<u8> = vec![
                    40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0,
                    0, 0, 0, 5, 116, 101, 115, 116, 115, 0,
                ];
                Ok(patch_bytes)
            },
            |_url, _event| Ok(()),
        );
        // There is an update available.
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));

        // Go ahead and do the update.
        shorebird_update();

        assert_eq!(shorebird_current_boot_patch_number(), 0);
        assert_eq!(shorebird_next_boot_patch_number(), 1);

        // Read path contents into memory and check against expected.
        let c_path = shorebird_next_boot_patch_path();
        let path = to_rust(c_path).unwrap();
        unsafe { shorebird_free_string(c_path) };
        let new = std::fs::read_to_string(path).unwrap();
        assert_eq!(new, expected_new);
    }

    #[serial]
    #[test]
    fn patch_success_with_result() -> anyhow::Result<()> {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        // Generated by `string_patch "hello world" "hello tests"`
        let base = "hello world";
        let expected_new: &str = "hello tests";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // set up the network hooks to return a patch.
        testing_set_network_hooks(
            |_url, request| {
                assert_eq!(request.channel, "beta");
                // Generated by `string_patch "hello world" "hello tests"`
                let hash = "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45";
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url| {
                // Generated by `string_patch "hello world" "hello tests"`
                let patch_bytes: Vec<u8> = vec![
                    40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0,
                    0, 0, 0, 5, 116, 101, 115, 116, 115, 0,
                ];
                Ok(patch_bytes)
            },
            |_url, _event| Ok(()),
        );
        // There is an update available.
        let channel_c_str = allocate_c_string("beta")?;
        assert!(shorebird_check_for_downloadable_update(channel_c_str));

        // Go ahead and do the update.
        let result = shorebird_update_with_result(channel_c_str);
        unsafe { shorebird_free_string(channel_c_str) };

        unsafe {
            assert_eq!(result.read().status, SHOREBIRD_UPDATE_INSTALLED);
            shorebird_free_update_result(result as *mut UpdateResult);
        }
        assert_eq!(shorebird_current_boot_patch_number(), 0);
        assert_eq!(shorebird_next_boot_patch_number(), 1);

        shorebird_validate_next_boot_patch();
        // Read path contents into memory and check against expected.
        let c_path = shorebird_next_boot_patch_path();
        let path = to_rust(c_path).unwrap();
        unsafe { shorebird_free_string(c_path) };
        let new = std::fs::read_to_string(path).unwrap();
        assert_eq!(new, expected_new);

        Ok(())
    }

    #[serial]
    #[test]
    fn patch_check_no_patch_with_result() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        // Generated by `string_patch "hello world" "hello tests"`
        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // set up the network hooks to return a patch.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: false,
                    patch: None,
                    rolled_back_patch_numbers: None,
                })
            },
            |_url| Err(anyhow::anyhow!("Error")),
            |_url, _event| Ok(()),
        );

        // Go ahead and do the update.
        let result = shorebird_update_with_result(std::ptr::null());

        unsafe {
            assert_eq!(result.read().status, SHOREBIRD_NO_UPDATE);
            shorebird_free_update_result(result as *mut UpdateResult);
        }
    }

    #[serial]
    #[test]
    fn patch_check_failure_with_result() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        // Generated by `string_patch "hello world" "hello tests"`
        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // set up the network hooks to return a patch.
        testing_set_network_hooks(
            |_url, _request| Err(anyhow::anyhow!("Error")),
            |_url| {
                // Generated by `string_patch "hello world" "hello tests"`
                let patch_bytes: Vec<u8> = vec![
                    40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0,
                    0, 0, 0, 5, 116, 101, 115, 116, 115, 0,
                ];
                Ok(patch_bytes)
            },
            |_url, _event| Ok(()),
        );

        // Go ahead and do the update.
        let result = shorebird_update_with_result(std::ptr::null());

        unsafe {
            assert_eq!(result.read().status, SHOREBIRD_UPDATE_ERROR);
            shorebird_free_update_result(result as *mut UpdateResult);
        }
    }

    #[serial]
    #[test]
    fn patch_download_failure_with_result() -> anyhow::Result<()> {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        // Generated by `string_patch "hello world" "hello tests"`
        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // set up the network hooks to return a patch.
        testing_set_network_hooks(
            |_url, request| {
                // shorebird_update_with_result was called with the beta channel, ensure that is
                // piped through to the network request.
                assert_eq!(request.channel, "beta");

                // Generated by `string_patch "hello world" "hello tests"`
                let hash = "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45";
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url| Err(anyhow::anyhow!("Error")),
            |_url, _event| Ok(()),
        );

        // Go ahead and do the update.
        let channel_c_str = allocate_c_string("beta")?;
        let result = shorebird_update_with_result(channel_c_str);
        unsafe { shorebird_free_string(channel_c_str) };

        unsafe {
            assert_eq!(result.read().status, SHOREBIRD_UPDATE_ERROR);
            shorebird_free_update_result(result as *mut UpdateResult);
        }

        Ok(())
    }

    #[serial]
    #[test]
    fn current_boot_patch_set_after_reporting_launch_start() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        // Generated by `string_patch "hello world" "hello tests"`
        let base = "hello world";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(apk_path.to_str().unwrap(), base.as_bytes());
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // set up the network hooks to return a patch.
        testing_set_network_hooks(
            |_url, _request| {
                // Generated by `string_patch "hello world" "hello tests"`
                let hash = "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45";
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url| {
                // Generated by `string_patch "hello world" "hello tests"`
                let patch_bytes: Vec<u8> = vec![
                    40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0,
                    0, 0, 0, 5, 116, 101, 115, 116, 115, 0,
                ];
                Ok(patch_bytes)
            },
            |_url, _event| Ok(()),
        );

        // Ensure we start with no current patch
        assert_eq!(shorebird_current_boot_patch_number(), 0);

        // There is an update available.
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));
        // Go ahead and do the update.
        shorebird_update();

        // Ensure we have not yet updated the current patch.
        assert_eq!(shorebird_current_boot_patch_number(), 0);

        shorebird_report_launch_start();

        // After reporting a launch start, the next boot patch should be the current patch.
        assert_eq!(shorebird_current_boot_patch_number(), 1);

        shorebird_report_launch_success();

        // After reporting a launch success, the current patch number should not have changed.
        assert_eq!(shorebird_current_boot_patch_number(), 1);
    }

    #[serial]
    #[test]
    fn forgot_init() {
        testing_reset_config();
        shorebird_validate_next_boot_patch();
        assert_eq!(shorebird_next_boot_patch_number(), 0);
        assert_eq!(shorebird_next_boot_patch_path(), null_mut());
    }

    #[serial]
    #[test]
    fn init_twice() {
        // It should only be possible to init once per process.
        // Successive calls should log a warning, but not hang or crash.
        // This is slightly different as a unit test because we use a
        // thread local for the storage, but it should test the same idea.

        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        let fake_libapp_path = tmp_dir.path().join("lib/arch/libapp.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        let fake_libapp_path = tmp_dir.path().join("lib/arch/libapp.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: bar");

        // This will return false because we have already initialized.
        assert!(!shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);
    }

    #[serial]
    #[test]
    fn usage_during_hung_update() {
        // It should be possible to call into shorebird, even when an
        // background update thread may be waiting a long time on a network
        // request.

        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        let fake_libapp_path = tmp_dir.path().join("lib/arch/libapp.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        use std::sync::Mutex;
        static CALLBACK_MUTEX: Mutex<u32> = Mutex::new(0);
        // static WAIT_PAIR: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

        // set up the network hooks to return a patch.
        testing_set_network_hooks(
            |_url: &str, _request| {
                // Hang until we have the lock.
                let _lock = CALLBACK_MUTEX.lock().unwrap();
                Ok(PatchCheckResponse {
                    patch_available: false,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: "ignored".to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url| {
                // Never called.
                Ok(Vec::new())
            },
            |_url, _event| Ok(()),
        );
        {
            // Lock the mutex before starting the thread.
            let _lock = CALLBACK_MUTEX.lock().unwrap();
            // Start our thread, which should hang on that lock.
            shorebird_start_update_thread();
            // Wait for the thread to start.
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert!(updater::update(None).is_err());
        }
        // Unlock the lock, and wait for the thread to finish.
        std::thread::sleep(std::time::Duration::from_millis(100));
        // Now we should be able to call into shorebird again.
        // assert!(updater::update().is_ok());
    }
}
