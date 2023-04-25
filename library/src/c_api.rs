// This file handles translating the updater library's types into C types.

// Currently manually prefixing all functions with "shorebird_" to avoid
// name collisions with other libraries.
// cbindgen:prefix-with-name could do this for us.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use anyhow::Context;

use crate::updater;

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
fn log_on_error<F, R>(f: F, context: &str, default: R) -> R
where
    F: FnOnce() -> Result<R, anyhow::Error>,
{
    let result = f();
    match result {
        Ok(r) => r,
        Err(e) => {
            error!("Error {}: {:?}", context, e);
            default
        }
    }
}

/// Configures updater.  First parameter is a struct containing configuration
/// from the running app.  Second parameter is a YAML string containing
/// configuration compiled into the app.
#[no_mangle]
pub extern "C" fn shorebird_init(c_params: *const AppParameters, c_yaml: *const libc::c_char) {
    log_on_error(
        || {
            let config = app_config_from_c(c_params)?;
            let yaml_string = to_rust(c_yaml)?;
            let result = updater::init(config, &yaml_string)?;
            Ok(result)
        },
        "initializing updater",
        (),
    )
}

/// Return the active patch number, or NULL if there is no active patch.
#[no_mangle]
pub extern "C" fn shorebird_next_boot_patch_number() -> *mut c_char {
    log_on_error(
        || {
            let patch = updater::next_boot_patch().context("failed to fetch patch info")?;
            allocate_c_string(&patch.number.to_string())
        },
        "fetching next_boot_patch_number",
        std::ptr::null_mut(),
    )
}

/// Return the path to the active patch for the app, or NULL if there is no
/// active patch.
#[no_mangle]
pub extern "C" fn shorebird_next_boot_patch_path() -> *mut c_char {
    log_on_error(
        || {
            let patch = updater::next_boot_patch().context("failed to fetch patch info")?;
            allocate_c_string(&patch.path)
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
    return updater::check_for_update();
}

/// Synchronously download an update if one is available.
#[no_mangle]
pub extern "C" fn shorebird_update() {
    updater::update();
}

/// Start a thread to download an update if one is available.
#[no_mangle]
pub extern "C" fn shorebird_start_update_thread() {
    updater::start_update_thread();
}

/// Tell the updater that we're launching from what it told us was the
/// next patch to boot from.  This will copy the next_boot patch to be
/// the current_boot patch.
/// It is required to call this function before calling
/// shorebird_report_launch_success or shorebird_report_launch_failure.
#[no_mangle]
pub extern "C" fn shorebird_report_launch_start() {
    log_on_error(
        || updater::report_launch_start(),
        "reporting launch start",
        (),
    );
}

/// Report that the app failed to launch.  This will cause the updater to
/// attempt to roll back to the previous version if this version has not
/// been launched successfully before.
#[no_mangle]
pub extern "C" fn shorebird_report_launch_failure() {
    log_on_error(
        || {
            updater::report_launch_failure()?;
            Ok(())
        },
        "reporting launch failure",
        (),
    );
}

/// Report that the app launched successfully.  This will mark the current
/// as having been launched successfully.  We don't currently do anything
/// with this information, but it could be used to record a point at which
/// we will not roll back from.
/// This is not currently wired up to be called from the Engine.  It's unclear
/// where best to connect it.  Expo waits 5 seconds after the app launches
/// and then marks the launch as successful.  We could do something similar.
#[no_mangle]
pub extern "C" fn shorebird_report_launch_success() {
    log_on_error(
        || {
            updater::report_launch_success()?;
            Ok(())
        },
        "reporting launch success",
        (),
    );
}
