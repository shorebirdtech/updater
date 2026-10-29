//! Test-only C symbols layered on top of `updater`.
//!
//! This crate exists so Dart integration tests under
//! `shorebird_code_push/test/integration/` can drive the updater
//! end-to-end without bloating the production C API or adding
//! `#[cfg(test)]` symbols to the cdylib that ships in the engine.
//!
//! The crate produces a single `cdylib` artifact
//! (`libupdater_test_hooks.{dylib,so}` / `updater_test_hooks.dll`) that
//! exposes:
//!
//! 1. The production C surface from `updater::c_api::dart` and
//!    `updater::c_api::engine`, re-exported so Dart tests can drive a
//!    real `shorebird_init` / `shorebird_update_with_result` cycle
//!    against tempdir-scoped state.
//! 2. Extra `shorebird_test_*` symbols defined here that wrap
//!    Rust-internal items in `updater` (gated behind the `test-hooks`
//!    Cargo feature) — currently just `shorebird_test_reset`, with more
//!    to come as later stages of the integration suite need them.
//!
//! Production updater builds (the cdylib/staticlib that ships in the
//! engine) do not enable `test-hooks` and never link this crate.

use std::ffi::CStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::Mutex;

use updater::c_api::engine;

// Re-export the production C API. This makes the symbols part of this
// crate's public API surface, which prevents the linker from stripping
// them when producing the cdylib (an rlib's `#[no_mangle]` items are
// otherwise eligible for DCE because they aren't roots from this
// crate's perspective).
#[allow(unused_imports)]
pub use updater::c_api::dart::*;
#[allow(unused_imports)]
pub use updater::c_api::engine::*;

/// Resets the updater's global config so the next `shorebird_init`
/// starts from scratch. Equivalent to a process restart for state
/// purposes — Dart tests call this between scenarios so each test
/// runs against a fresh updater.
#[no_mangle]
pub extern "C" fn shorebird_test_reset() {
    updater::testing_reset_config();
}

// `FileCallbacks` that serve the bytes of the `libapp_path` passed to
// `shorebird_test_init`, the way a real engine serves the patch base. macOS
// builds read the base through these callbacks, and the test patch fixtures
// copy from it, so they must deliver the file's real bytes.
static BASE_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

fn set_base_path(libapp_path: *const c_char) {
    let path = unsafe { CStr::from_ptr(libapp_path) }.to_string_lossy();
    *BASE_PATH.lock().unwrap() = Some(PathBuf::from(path.as_ref()));
}

extern "C" fn base_open() -> *mut libc::c_void {
    let path = BASE_PATH.lock().unwrap().clone();
    match path.map(File::open) {
        Some(Ok(file)) => Box::into_raw(Box::new(file)).cast(),
        _ => std::ptr::null_mut(),
    }
}
extern "C" fn base_read(handle: *mut libc::c_void, buffer: *mut u8, count: usize) -> usize {
    let file = unsafe { &mut *handle.cast::<File>() };
    let buf = unsafe { std::slice::from_raw_parts_mut(buffer, count) };
    file.read(buf).unwrap_or(0)
}
extern "C" fn base_seek(handle: *mut libc::c_void, offset: i64, whence: i32) -> i64 {
    let file = unsafe { &mut *handle.cast::<File>() };
    let pos = match whence {
        libc::SEEK_SET => SeekFrom::Start(offset as u64),
        libc::SEEK_CUR => SeekFrom::Current(offset),
        libc::SEEK_END => SeekFrom::End(offset),
        _ => return -1,
    };
    file.seek(pos).map_or(-1, |p| p as i64)
}
extern "C" fn base_close(handle: *mut libc::c_void) {
    drop(unsafe { Box::from_raw(handle.cast::<File>()) });
}

/// Test-only convenience wrapper around `shorebird_init` that builds
/// `AppParameters` and `FileCallbacks` serving `libapp_path` internally. Dart tests
/// pass plain C strings instead of needing bindings for those
/// engine-API structs.
#[no_mangle]
pub extern "C" fn shorebird_test_init(
    app_storage_dir: *const c_char,
    code_cache_dir: *const c_char,
    release_version: *const c_char,
    libapp_path: *const c_char,
    yaml: *const c_char,
) -> bool {
    set_base_path(libapp_path);
    let libapp_paths = [libapp_path];
    let params = engine::AppParameters {
        release_version,
        original_libapp_paths: libapp_paths.as_ptr(),
        original_libapp_paths_size: 1,
        app_storage_dir,
        code_cache_dir,
    };
    let callbacks = engine::FileCallbacks {
        open: base_open,
        read: base_read,
        seek: base_seek,
        close: base_close,
    };
    engine::shorebird_init(&params, callbacks, yaml)
}

/// Simulates the engine's successful boot of `next_boot_patch`.
/// In production this is two engine actions (launch-start, then
/// launch-success after Dart VM startup completes); the Dart layer
/// has no concept of either, so we expose the combined outcome as a
/// single semantic action: "the next patch booted cleanly."
#[no_mangle]
pub extern "C" fn shorebird_test_simulate_successful_launch() {
    engine::shorebird_report_launch_start();
    engine::shorebird_report_launch_success();
}
