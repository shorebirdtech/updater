//! Dart-stable C API surface.
//!
//! These symbols are consumed by `package:shorebird_code_push` via FFI. The
//! header generated for this module (`include/updater_dart.h`) is the input
//! to ffigen, so anything declared here is part of the package's public ABI.
//! Do not break changes here without bumping the package version.
//!
//! cbindgen reads this file directly (see `build.rs`) and emits exactly the
//! `pub extern "C"` items found here plus the types they reference. Adding
//! a function to this file automatically adds it to the Dart header — no
//! cbindgen-config update required. Conversely, anything declared in
//! `c_api::engine` cannot leak into this header.
//!
//! For symbols consumed only by Shorebird's Flutter engine, see
//! `c_api::engine` — that surface is unstable and changes freely.
use std::os::raw::c_char;

use super::{allocate_c_string, free_c_string, log_on_error, to_rust_option};
use crate::{updater, UpdateStatus};

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

/// Another update was already in progress when this call was made. The
/// already-running update will continue; the caller did not start a new one.
/// This is a benign outcome, not an error.
pub const SHOREBIRD_UPDATE_IN_PROGRESS: i32 = 4;

#[repr(C)]
pub struct UpdateResult {
    pub status: i32,
    pub message: *const libc::c_char,
}

fn to_update_result(status: anyhow::Result<UpdateStatus>) -> UpdateResult {
    match status {
        Ok(status) => {
            let message = status.to_string();
            UpdateResult {
                status: status as i32,
                message: allocate_c_string(message.as_str()).unwrap_or(std::ptr::null_mut()),
            }
        }
        Err(err) => UpdateResult {
            status: SHOREBIRD_UPDATE_ERROR,
            message: allocate_c_string(&err.to_string()).unwrap_or(std::ptr::null_mut()),
        },
    }
}

/// The currently running patch number, or 0 if the release has not been
/// patched. The internal name for this concept is `running_patch`; the
/// FFI symbol keeps the historical `current_boot_patch_number` spelling
/// because the Flutter Engine and existing pub releases of
/// `shorebird_code_push` link against it.
#[no_mangle]
pub extern "C" fn shorebird_current_boot_patch_number() -> usize {
    log_on_error(
        || Ok(updater::running_patch()?.map_or(0, |p| p.number)),
        "fetching running_patch_number",
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

/// Synchronously download an update on the first non-null channel of:
///   1. `c_channel`
///   2. The channel specified in shorebird.yaml
///   3. The default "stable" channel
///
/// Returns an [UpdateResult] indicating whether the update was successful.
#[no_mangle]
pub extern "C" fn shorebird_update_with_result(c_channel: *const c_char) -> *const UpdateResult {
    let result = match to_rust_option(c_channel) {
        Ok(channel) => to_update_result(updater::update(channel.as_deref())),
        Err(err) => to_update_result(Err(err)),
    };
    Box::into_raw(Box::new(result))
}

/// Frees an `UpdateResult` previously returned by
/// `shorebird_update_with_result`. Frees the embedded `message` string and
/// the result allocation itself.
///
/// # Safety
///
/// `result` must be a valid pointer returned by `shorebird_update_with_result`,
/// or null (in which case this is a no-op).
#[no_mangle]
pub unsafe extern "C" fn shorebird_free_update_result(result: *mut UpdateResult) {
    if result.is_null() {
        return;
    }
    let result = unsafe { Box::from_raw(result) };
    unsafe { free_c_string(result.message) };
}

/// Requests that the Flutter engine restart the app's Dart code without
/// killing the host process, booting from whatever patch the updater has
/// selected as the next boot patch (or the base release if none). This is
/// how a downloaded patch is applied without the user relaunching the app.
///
/// Returns true if the engine accepted the request and will restart
/// momentarily; Dart code continues running until the restart begins, so
/// callers should not assume execution stops at this call. Returns false if
/// restarting is not supported (engine too old, debug build, or an
/// unsupported engine configuration such as multiple concurrent engines).
#[no_mangle]
pub extern "C" fn shorebird_restart_app() -> bool {
    crate::restart::request_restart()
}
