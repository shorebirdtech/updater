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

use std::os::raw::c_char;

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

// Stub `FileCallbacks` mirroring the `#[cfg(test)]` `FileCallbacks::new`
// in `library/src/c_api/c_file.rs`. The test patch fixtures are
// self-contained zstd payloads that bipatch can apply against an empty
// source, so the read/seek callbacks never need to deliver real bytes.
extern "C" fn stub_open() -> *mut libc::c_void {
    // `CFileProvider` only checks for null to detect open failure, so
    // any non-null value works. `NonNull::dangling()` gives us one
    // without tripping clippy's `manual_dangling_ptr` lint (which fires
    // on integer-to-pointer casts like `1 as *mut _`).
    std::ptr::NonNull::<libc::c_void>::dangling().as_ptr()
}
extern "C" fn stub_read(_handle: *mut libc::c_void, _buffer: *mut u8, _count: usize) -> usize {
    0
}
extern "C" fn stub_seek(_handle: *mut libc::c_void, _offset: i64, _whence: i32) -> i64 {
    0
}
extern "C" fn stub_close(_handle: *mut libc::c_void) {}

/// Test-only convenience wrapper around `shorebird_init` that builds
/// `AppParameters` and stub `FileCallbacks` internally. Dart tests
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
    let libapp_paths = [libapp_path];
    let params = engine::AppParameters {
        release_version,
        original_libapp_paths: libapp_paths.as_ptr(),
        original_libapp_paths_size: 1,
        app_storage_dir,
        code_cache_dir,
    };
    let callbacks = engine::FileCallbacks {
        open: stub_open,
        read: stub_read,
        seek: stub_seek,
        close: stub_close,
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

// Fake engine restart handler state. Mirrors what the Shorebird engine
// does in production: it registers a handler via
// `shorebird_set_restart_handler` at startup, and `shorebird_restart_app`
// (called by `package:shorebird_code_push`) relays to it.
static RESTART_REQUEST_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static RESTART_ACCEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

extern "C" fn test_restart_handler() -> bool {
    RESTART_REQUEST_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    RESTART_ACCEPT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Installs a fake engine restart handler, as the Shorebird engine does
/// at startup. `accept` controls whether the handler reports the restart
/// as scheduled (the value returned to `shorebird_restart_app` callers).
/// Also resets the request counter. `shorebird_test_reset` removes the
/// handler again.
#[no_mangle]
pub extern "C" fn shorebird_test_install_restart_handler(accept: bool) {
    RESTART_ACCEPT.store(accept, std::sync::atomic::Ordering::SeqCst);
    RESTART_REQUEST_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
    engine::shorebird_set_restart_handler(Some(test_restart_handler));
}

/// The number of restart requests the fake handler has received since it
/// was last installed.
#[no_mangle]
pub extern "C" fn shorebird_test_restart_request_count() -> usize {
    RESTART_REQUEST_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}
