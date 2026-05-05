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
