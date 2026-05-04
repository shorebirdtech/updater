//! Engine-internal C API surface.
//!
//! These symbols are consumed by Shorebird's Flutter engine
//! (`flutter::shorebird::Updater` in `shell/common/shorebird/updater.cc`) and
//! by no other public consumer. Both sides ship together as part of the
//! engine, so this surface has no stability guarantee — change freely as the
//! engine integration evolves.
//!
//! cbindgen reads this file directly (see `build.rs`) and emits exactly the
//! `pub extern "C"` items found here plus the types they reference. Adding
//! a function to this file automatically adds it to the engine header — no
//! cbindgen-config update required. Conversely, anything declared in
//! `c_api::dart` cannot leak into this header.
//!
//! For the stable Dart surface consumed by `package:shorebird_code_push`,
//! see `c_api::dart`.
use std::os::raw::c_char;
use std::path::PathBuf;

use super::{allocate_c_string, free_c_string, log_on_error, to_rust};
use crate::c_api::c_file::CFileProvider;
use crate::updater;

/// Struct containing configuration parameters for the updater.
/// Passed to `shorebird_init`.
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

fn path_to_c_string(path: Option<PathBuf>) -> anyhow::Result<*mut c_char> {
    Ok(match path {
        Some(v) => allocate_c_string(v.to_str().unwrap())?,
        None => std::ptr::null_mut(),
    })
}

/// Free a string returned by the updater library.
///
/// # Safety
///
/// If this function is called with a non-null pointer, it must be a pointer
/// returned by the updater library.
#[no_mangle]
pub unsafe extern "C" fn shorebird_free_string(c_string: *const c_char) {
    unsafe { free_c_string(c_string) }
}

/// Configures the updater. First parameter is a struct containing
/// configuration from the running app. Second parameter is a YAML string
/// containing configuration compiled into the app. Returns true on success
/// and false on failure. If false is returned, the updater library will not
/// be usable.
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

/// Performs integrity checks on the next boot patch. If the patch fails
/// these checks, the patch will be deleted and the next boot patch will be
/// set to the last successfully booted patch or the base release if there is
/// no last successfully booted patch.
#[no_mangle]
pub extern "C" fn shorebird_validate_next_boot_patch() {
    log_on_error(
        updater::validate_next_boot_patch,
        "validating next_boot_patch",
        (),
    );
}

/// The path to the patch that will boot on the next run of the app, or NULL
/// if there is no next patch. The caller must free the returned string with
/// `shorebird_free_string`.
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

/// Report that the app failed to launch. This will cause the updater to
/// attempt to roll back to the previous version if this version has not been
/// launched successfully before.
#[no_mangle]
pub extern "C" fn shorebird_report_launch_failure() {
    log_on_error(
        updater::report_launch_failure,
        "reporting launch failure",
        (),
    );
}

/// Report that the app launched successfully. The Shell constructor calls
/// this once per process when the VM has finished booting; it pairs with
/// `shorebird_report_launch_start` to mark a patch as having booted cleanly.
#[no_mangle]
pub extern "C" fn shorebird_report_launch_success() {
    log_on_error(
        updater::report_launch_success,
        "reporting launch success",
        (),
    );
}
