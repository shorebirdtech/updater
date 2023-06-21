// This file handles translating the updater library's types into C types.

// Currently manually prefixing all functions with "shorebird_" to avoid
// name collisions with other libraries.
// cbindgen:prefix-with-name could do this for us.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::updater;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as error}; // Workaround to use println! for logs.

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

    /// Path to cache_dir where the updater will store downloaded artifacts.
    pub cache_dir: *const libc::c_char,
}

/// Converts a C string to a Rust string, does not free the C string.
fn to_rust(c_string: *const libc::c_char) -> anyhow::Result<String> {
    anyhow::ensure!(!c_string.is_null(), "Null string passed to to_rust");
    let c_str = unsafe { CStr::from_ptr(c_string) };
    Ok(c_str.to_str()?.to_string())
}

/// Converts a Rust string to a C string, caller must free the C string.
fn allocate_c_string(rust_string: &str) -> anyhow::Result<*mut c_char> {
    let c_str = CString::new(rust_string)?;
    Ok(c_str.into_raw())
}

/// Converts a Rust string to a C string, caller must free the C string.
fn to_c_string(maybe_string: Option<String>) -> anyhow::Result<*mut c_char> {
    Ok(match maybe_string {
        Some(v) => allocate_c_string(&v)?,
        None => std::ptr::null_mut(),
    })
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
        cache_dir: to_rust(c_params_ref.cache_dir)?,
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
        error!("Error {}: {:?}", context, e);
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
    c_yaml: *const libc::c_char,
) -> bool {
    log_on_error(
        || {
            let config = app_config_from_c(c_params)?;
            let yaml_string = to_rust(c_yaml)?;
            updater::init(config, &yaml_string)?;
            Ok(true)
        },
        "initializing updater",
        false,
    )
}

/// The currently running patch number, or 0 if the release has not been
/// patched.
#[no_mangle]
pub extern "C" fn shorebird_current_boot_patch_number() -> usize {
    log_on_error(
        || {
            Ok(updater::current_boot_patch()?
                .map(|p| p.number)
                .unwrap_or(0))
        },
        "fetching next_boot_patch_number",
        0,
    )
}

/// The patch number that will boot on the next run of the app, or 0 if there is
/// no next patch.
#[no_mangle]
pub extern "C" fn shorebird_next_boot_patch_number() -> usize {
    log_on_error(
        || Ok(updater::next_boot_patch()?.map(|p| p.number).unwrap_or(0)),
        "fetching next_boot_patch_number",
        0,
    )
}

/// The path to the patch that will boot on the next run of the app, or NULL if
/// there is no next patch.
#[no_mangle]
pub extern "C" fn shorebird_next_boot_patch_path() -> *mut c_char {
    log_on_error(
        || {
            let maybe_path = updater::next_boot_patch()?.map(|p| p.path);
            to_c_string(maybe_path)
        },
        "fetching next_boot_patch_path",
        std::ptr::null_mut(),
    )
}

/// Free a string returned by the updater library.
#[no_mangle]
pub extern "C" fn shorebird_free_string(c_string: *mut c_char) {
    if c_string.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(c_string));
    }
}

/// Check for an update.  Returns true if an update is available.
#[no_mangle]
pub extern "C" fn shorebird_check_for_update() -> bool {
    log_on_error(updater::check_for_update, "checking for update", false)
}

/// Synchronously download an update if one is available.
#[no_mangle]
pub extern "C" fn shorebird_update() {
    log_on_error(
        || updater::update().and_then(|result| Ok(info!("Update result: {}", result))),
        "downloading update",
        (),
    );
}

/// Start a thread to download an update if one is available.
#[no_mangle]
pub extern "C" fn shorebird_start_update_thread() {
    updater::start_update_thread();
}

/// Tell the updater that we're launching from what it told us was the
/// next patch to boot from. This will copy the next_boot patch to be the
/// current_boot patch.
///
/// It is required to call this function before calling
/// shorebird_report_launch_success or shorebird_report_launch_failure.
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
        network::PatchCheckResponse, testing_set_network_hooks, updater::testing_reset_config,
    };
    use tempdir::TempDir;

    use std::{ffi::CString, ptr::null_mut};

    pub fn c_string(string: &str) -> *mut libc::c_char {
        let c_string = CString::new(string).unwrap().into_raw();
        c_string
    }

    pub fn free_c_string(string: *mut libc::c_char) {
        unsafe {
            drop(CString::from_raw(string));
        }
    }

    pub fn c_array(strings: Vec<String>) -> *mut *mut libc::c_char {
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

    pub fn free_c_array(strings: *mut *mut libc::c_char, size: usize) {
        let v = unsafe { Vec::from_raw_parts(strings, size, size) };

        // Now drop one string at a time.
        for string in v {
            free_c_string(string);
        }
    }

    pub fn parameters(tmp_dir: &TempDir, base_path: &str) -> super::AppParameters {
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
        let app_paths_vec = vec![base_path.to_owned()];
        let app_paths_size = app_paths_vec.len() as i32;
        let app_paths = c_array(app_paths_vec);

        super::AppParameters {
            cache_dir: c_string(&cache_dir),
            release_version: c_string("1.0.0"),
            original_libapp_paths: app_paths as *const *const libc::c_char,
            original_libapp_paths_size: app_paths_size,
        }
    }

    pub fn free_parameters(params: super::AppParameters) {
        free_c_string(params.cache_dir as *mut libc::c_char);
        free_c_string(params.release_version as *mut libc::c_char);
        free_c_array(
            params.original_libapp_paths as *mut *mut libc::c_char,
            params.original_libapp_paths_size as usize,
        )
    }

    #[test]
    fn init_with_nulls() {
        testing_reset_config();
        // Should log but not crash.
        assert_eq!(shorebird_init(std::ptr::null(), std::ptr::null()), false);
    }

    #[test]
    fn init_with_null_app_parameters() {
        testing_reset_config();
        // Should log but not crash.
        let c_params = AppParameters {
            cache_dir: std::ptr::null(),
            release_version: std::ptr::null(),
            original_libapp_paths: std::ptr::null(),
            original_libapp_paths_size: 0,
        };
        assert_eq!(shorebird_init(&c_params, std::ptr::null()), false);
    }

    #[test]
    fn init_with_bad_yaml() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();
        let c_params = parameters(&tmp_dir, "libapp.so");
        let c_yaml = c_string("bad yaml");
        assert_eq!(shorebird_init(&c_params, c_yaml), false);
        free_c_string(c_yaml);
        free_parameters(c_params);
    }

    #[test]
    fn empty_state_no_update() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();
        let c_params = parameters(&tmp_dir, "libapp.so");
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert_eq!(shorebird_init(&c_params, c_yaml), true);
        free_c_string(c_yaml);
        free_parameters(c_params);

        // Number is 0 and path is empty (but do not crash) when we have an
        // empty cache and update has not been called.
        assert_eq!(shorebird_next_boot_patch_number(), 0);
        assert_eq!(shorebird_next_boot_patch_path(), null_mut());

        // Similarly we can report launches with no patch without crashing.
        shorebird_report_launch_start();
        shorebird_report_launch_success();
        shorebird_report_launch_failure();
    }

    fn write_fake_zip(zip_path: &str, libapp_contents: &[u8]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(zip_path).unwrap());
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o755);
        let app_path = crate::android::get_relative_lib_path("libapp.so");
        zip.start_file(app_path.to_str().unwrap(), options).unwrap();
        zip.write_all(libapp_contents).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn patch_success() {
        testing_reset_config();
        let tmp_dir = TempDir::new("example").unwrap();

        // Generated by `string_patch "hello world" "hello tests"`
        let base = "hello world";
        let expected_new: &str = "hello tests";
        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_zip(apk_path.to_str().unwrap(), base.as_bytes());
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert_eq!(shorebird_init(&c_params, c_yaml), true);
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
                    }),
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
        );
        shorebird_update();

        let version = shorebird_next_boot_patch_number();
        assert_eq!(version, 1);

        // Read path contents into memory and check against expected.
        let path = to_rust(shorebird_next_boot_patch_path()).unwrap();
        let new = std::fs::read_to_string(path).unwrap();
        assert_eq!(new, expected_new);
    }

    #[test]
    fn init_twice() {
        // It should only be possible to init once per process.
        // Successive calls should log a warning, but not hang or crash.
        // This may be difficult to test because in unit tests we use a
        // thread_local to reset the config between tests.
    }

    #[test]
    fn usage_during_hung_update() {
        // It should be possible to call into shorebird, even when an
        // background update thread may be waiting a long time on a network
        // request.
    }

}
