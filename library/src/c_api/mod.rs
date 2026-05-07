// This module translates the updater library's types into C types.
//
// The C surface is split into two submodules, each fully self-contained:
//   - `dart`   — stable surface consumed by `package:shorebird_code_push`
//                (header: `include/updater_dart.h`, driven by ffigen).
//   - `engine` — surface consumed only by Shorebird's Flutter engine
//                (header: `include/updater_engine.h`, no stability guarantee).
//
// Each submodule defines the `pub extern "C"` items it exports plus any C
// types those items reference. cbindgen scans the submodule files directly
// (see `build.rs`) — there is no cross-bucket exclusion list, and a new
// function added to one bucket cannot leak into the other bucket's header.
//
// Items in this file are private Rust helpers shared between the two
// buckets. They are not `extern "C"`, so cbindgen never emits them.
//
// Engine-side usage lives at `engine/src/flutter/shell/common/shorebird/updater.cc`
// in the Shorebird Flutter monorepo: <https://github.com/shorebirdtech/flutter>.
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

mod c_file;
pub mod dart;
pub mod engine;

#[cfg(test)]
pub use self::dart::*;
#[cfg(test)]
pub use self::engine::*;

/// Converts a C string to a Rust string, does not free the C string.
pub(super) fn to_rust(c_string: *const libc::c_char) -> anyhow::Result<String> {
    anyhow::ensure!(!c_string.is_null(), "Null string passed to to_rust");
    let c_str = unsafe { CStr::from_ptr(c_string) };
    Ok(c_str.to_str()?.to_string())
}

pub(super) fn to_rust_option(c_string: *const c_char) -> anyhow::Result<Option<String>> {
    if c_string.is_null() {
        return Ok(None);
    }
    Ok(Some(to_rust(c_string)?))
}

/// Converts a Rust string to a C string, caller must free the C string.
pub(super) fn allocate_c_string(rust_string: &str) -> anyhow::Result<*mut c_char> {
    let c_str = CString::new(rust_string)?;
    Ok(c_str.into_raw())
}

/// Drops a C string previously allocated by `allocate_c_string`. No-op on
/// null. Callable by both buckets — `engine::shorebird_free_string` and
/// `dart::shorebird_free_update_result` both delegate here so the
/// CString-from-raw unsafe ownership logic lives in one place.
///
/// # Safety
///
/// `c_string` must be null or a pointer previously returned by
/// `allocate_c_string` and not yet freed.
pub(super) unsafe fn free_c_string(c_string: *const c_char) {
    if c_string.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(c_string as *mut c_char));
    }
}

/// Helper function to log errors instead of panicking or returning a result.
pub(super) fn log_on_error<F, R>(f: F, context: &str, error_result: R) -> R
where
    F: FnOnce() -> Result<R, anyhow::Error>,
{
    f().unwrap_or_else(|e| {
        shorebird_error!("Error {}: {:?}", context, e);
        error_result
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        network::{
            testing_set_network_hooks, DownloadResult, PatchCheckResponse, UNEXPECTED_DOWNLOAD,
            UNEXPECTED_REPORT,
        },
        test_utils::write_fake_apk,
        updater,
    };
    use anyhow::Ok;
    use serial_test::serial;
    use std::path::Path;
    use tempfile::TempDir;
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
    fn parameters(tmp_dir: &TempDir, libapp_path: &str) -> AppParameters {
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
        let app_paths_vec = vec![libapp_path.to_owned()];
        let app_paths_size = app_paths_vec.len() as i32;
        let app_paths = c_array(app_paths_vec);

        AppParameters {
            app_storage_dir: c_string(&cache_dir),
            code_cache_dir: c_string(&cache_dir),
            release_version: c_string("1.0.0"),
            original_libapp_paths: app_paths as *const *const libc::c_char,
            original_libapp_paths_size: app_paths_size,
        }
    }

    fn free_parameters(params: AppParameters) {
        free_c_string(params.app_storage_dir as *mut libc::c_char);
        free_c_string(params.code_cache_dir as *mut libc::c_char);
        free_c_string(params.release_version as *mut libc::c_char);
        free_c_array(
            params.original_libapp_paths as *mut *mut libc::c_char,
            params.original_libapp_paths_size as usize,
        )
    }

    /// Run `shorebird_update_with_result` with the given channel, assert the
    /// status equals `expected`, then free the result. Replaces uses of the
    /// retired `shorebird_update()` helper inside tests that don't otherwise
    /// inspect the result.
    fn run_update_expecting(channel: *const c_char, expected: i32) {
        let result = shorebird_update_with_result(channel);
        unsafe {
            assert_eq!(result.read().status, expected);
            shorebird_free_update_result(result as *mut UpdateResult);
        }
    }

    /// A precomputed bidiff patch artifact along with the inputs that
    /// produced it. Generate one with:
    ///     cargo run --bin string_patch -- "<base>" "<new>"
    /// Then paste the four pieces into a `PatchFixture` constant:
    ///   - `base`: the `<base>` argument (must match the bytes
    ///     `write_fake_apk` writes for the test's fake APK)
    ///   - `new`: the `<new>` argument (the inflated content after
    ///     applying the patch — what tests assert against)
    ///   - `bytes`: the "Patch:" byte array
    ///   - `hash`: the "Hash (new):" sha256 hex of `new`
    ///
    /// All fixtures used together in a single test must share the same
    /// `base`, since `write_fake_apk` only writes one set of bytes.
    struct PatchFixture {
        base: &'static str,
        new: &'static str,
        hash: &'static str,
        bytes: &'static [u8],
    }

    impl PatchFixture {
        /// Helper for `testing_set_network_hooks` download callbacks: writes
        /// the fixture's patch bytes to `dest` and returns the matching
        /// `DownloadResult`.
        fn write_to(&self, dest: &Path) -> anyhow::Result<DownloadResult> {
            let total_bytes = self.bytes.len() as u64;
            std::fs::write(dest, self.bytes)?;
            Ok(DownloadResult {
                total_bytes,
                content_length: Some(total_bytes),
            })
        }
    }

    /// `string_patch "hello world" "hello tests"` — the default fixture
    /// used by tests that don't care which patch is which.
    const HELLO_TESTS_PATCH: PatchFixture = PatchFixture {
        base: "hello world",
        new: "hello tests",
        hash: "bb8f1d041a5cdc259055afe9617136799543e0a7a86f86db82f8c1fadbd8cc45",
        bytes: &[
            40, 181, 47, 253, 0, 128, 177, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0, 0, 0, 0,
            5, 116, 101, 115, 116, 115, 0,
        ],
    };

    /// `string_patch "hello world" "hello patch 2"` — distinct from
    /// `HELLO_TESTS_PATCH` for tests that need two non-equal artifacts
    /// (e.g. patch-to-patch rollback). Shares the same `base` so both
    /// fixtures can be used in the same test.
    const HELLO_PATCH_2_PATCH: PatchFixture = PatchFixture {
        base: "hello world",
        new: "hello patch 2",
        hash: "2bc806572d14496a1ddfbddf7ed7380fca87a80b739b2881544aba397d267c68",
        bytes: &[
            40, 181, 47, 253, 0, 128, 193, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 6, 0, 0, 0, 0, 0, 0,
            7, 112, 97, 116, 99, 104, 32, 50, 0,
        ],
    };

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

    /// Exercises the `to_rust_vector` failure path in
    /// `app_config_from_c` (engine.rs). All scalar fields are valid, but
    /// the libapp_paths array contains a null entry — `to_rust` rejects
    /// null and the `?` propagates. Without this test the array-conversion
    /// branch is never reached because `init_with_null_app_parameters`
    /// fails earlier on the scalar fields.
    #[serial]
    #[test]
    fn init_with_null_libapp_path_in_array() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();
        let cache_dir = tmp_dir.path().to_str().unwrap().to_string();

        let null_path: *const libc::c_char = std::ptr::null();
        let paths_array = [null_path];

        let c_params = AppParameters {
            app_storage_dir: c_string(&cache_dir),
            code_cache_dir: c_string(&cache_dir),
            release_version: c_string("1.0.0"),
            original_libapp_paths: paths_array.as_ptr(),
            original_libapp_paths_size: 1,
        };
        let c_yaml = c_string("app_id: foo");

        assert!(!shorebird_init(&c_params, FileCallbacks::new(), c_yaml));

        free_c_string(c_yaml);
        free_c_string(c_params.app_storage_dir as *mut libc::c_char);
        free_c_string(c_params.code_cache_dir as *mut libc::c_char);
        free_c_string(c_params.release_version as *mut libc::c_char);
    }

    /// Exercises the `Err` arm of the channel decode in
    /// `shorebird_update_with_result` (dart.rs). The channel pointer is
    /// non-null (so `to_rust_option` doesn't short-circuit to `Ok(None)`),
    /// but contains invalid UTF-8, so `CStr::to_str()` returns an error
    /// that propagates into `to_update_result(Err(_))`.
    #[serial]
    #[test]
    fn update_with_result_with_invalid_utf8_channel() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();
        let fake_libapp_path = tmp_dir.path().join("lib/arch/libapp.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // 0xFF is an invalid UTF-8 start byte. CStr::from_ptr accepts it
        // (C strings are null-terminated bytes, no encoding) but the
        // to_str() conversion fails — which is the path we're covering.
        let bad_bytes: [u8; 4] = [0xFF, 0xFE, 0xFD, 0];
        let result = shorebird_update_with_result(bad_bytes.as_ptr() as *const c_char);

        unsafe {
            assert_eq!(result.read().status, SHOREBIRD_UPDATE_ERROR);
            shorebird_free_update_result(result as *mut UpdateResult);
        }
    }

    #[serial]
    #[test]
    fn init_with_bad_yaml() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();
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
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
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
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_TESTS_PATCH.write_to(dest),
            |_url, _event| Ok(()),
        );
        // There is an update available.
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));

        // Go ahead and do the update.
        run_update_expecting(std::ptr::null(), SHOREBIRD_UPDATE_INSTALLED);

        assert_eq!(shorebird_current_boot_patch_number(), 0);
        assert_eq!(shorebird_next_boot_patch_number(), 1);

        // Read path contents into memory and check against expected.
        let c_path = shorebird_next_boot_patch_path();
        let path = to_rust(c_path).unwrap();
        unsafe { shorebird_free_string(c_path) };
        let new = std::fs::read_to_string(path).unwrap();
        assert_eq!(new, HELLO_TESTS_PATCH.new);
    }

    #[serial]
    #[test]
    fn patch_success_with_result() -> anyhow::Result<()> {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
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
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_TESTS_PATCH.write_to(dest),
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
        assert_eq!(new, HELLO_TESTS_PATCH.new);

        Ok(())
    }

    #[serial]
    #[test]
    fn patch_check_no_patch_with_result() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
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
            UNEXPECTED_DOWNLOAD,
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
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        // app_id is required or shorebird_init will fail.
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // set up the network hooks — patch check fails, so download should never be called.
        testing_set_network_hooks(
            |_url, _request| Err(anyhow::anyhow!("Error")),
            UNEXPECTED_DOWNLOAD,
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
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
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
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, _dest: &Path, _resume_from: u64| Err(anyhow::anyhow!("Error")),
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
    fn running_patch_set_after_reporting_launch_start() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
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
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_TESTS_PATCH.write_to(dest),
            |_url, _event| Ok(()),
        );

        // Ensure we start with no current patch
        assert_eq!(shorebird_current_boot_patch_number(), 0);

        // There is an update available.
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));
        // Go ahead and do the update.
        run_update_expecting(std::ptr::null(), SHOREBIRD_UPDATE_INSTALLED);

        // Ensure we have not yet updated the current patch.
        assert_eq!(shorebird_current_boot_patch_number(), 0);

        shorebird_report_launch_start();

        // After reporting a launch start, the next boot patch should be the current patch.
        assert_eq!(shorebird_current_boot_patch_number(), 1);

        shorebird_report_launch_success();

        // After reporting a launch success, the current patch number should not have changed.
        assert_eq!(shorebird_current_boot_patch_number(), 1);
    }

    /// Regression test for the patch-to-release rollback bug.
    /// Customer scenario: device is running patch 1, server rolls patch 1
    /// back to the base release (no replacement patch). After
    /// shorebird_check_for_downloadable_update processes the rollback,
    /// shorebird_current_boot_patch_number must still report 1 — the running
    /// process is still using patch 1 and needs to restart. Pair that with
    /// shorebird_next_boot_patch_number == 0 so callers can detect the
    /// "current != next" condition that signals restart_required.
    #[serial]
    #[test]
    fn rollback_to_release_keeps_running_patch() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // First, install patch 1 and report a successful launch.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_TESTS_PATCH.write_to(dest),
            |_url, _event| Ok(()),
        );

        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));
        run_update_expecting(std::ptr::null(), SHOREBIRD_UPDATE_INSTALLED);
        shorebird_report_launch_start();
        shorebird_report_launch_success();

        // Sanity: we are now running patch 1.
        assert_eq!(shorebird_current_boot_patch_number(), 1);
        assert_eq!(shorebird_next_boot_patch_number(), 1);

        // Now the server rolls back patch 1 with no replacement. The device
        // should fall back to the base release on the *next* boot, and the
        // running session should be told a restart is required.
        // Phase-1 spawned threads (PatchDownload, PatchInstallSuccess) hold a
        // clone of the config from when they were spawned, so they hit the old
        // report hook above. Nothing in phase 2 should report — only a
        // patch-check request happens — so use UNEXPECTED_REPORT to assert
        // that.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: false,
                    patch: None,
                    rolled_back_patch_numbers: Some(vec![1]),
                })
            },
            UNEXPECTED_DOWNLOAD,
            UNEXPECTED_REPORT,
        );

        // Server has no downloadable update — just the rollback signal.
        assert!(!shorebird_check_for_downloadable_update(std::ptr::null()));

        // The bug: pre-fix this returns 0. Post-fix it must return 1, because
        // the running process is still on patch 1.
        assert_eq!(shorebird_current_boot_patch_number(), 1);
        // Next boot has been cleared — the device will boot the release.
        assert_eq!(shorebird_next_boot_patch_number(), 0);
    }

    /// After a patch-to-release rollback, the next launch boots the base
    /// release. `running_patch` must reflect that — it cannot keep
    /// reporting the rolled-back patch from the previous run, or callers
    /// would see a perpetual `restartRequired`. The contract: a fresh
    /// process starts with `running_patch == None` (it's a session-scoped
    /// global, not persisted) and `report_launch_start` keeps it `None`
    /// because `next_boot_patch` is also `None`.
    #[serial]
    #[test]
    fn rollback_to_release_then_restart_clears_running_patch() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");

        // Phase 1: install patch 1 and report a successful launch.
        {
            let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
            let c_yaml = c_string("app_id: foo");
            assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
            free_c_string(c_yaml);
            free_parameters(c_params);
        }

        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_TESTS_PATCH.write_to(dest),
            |_url, _event| Ok(()),
        );
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));
        run_update_expecting(std::ptr::null(), SHOREBIRD_UPDATE_INSTALLED);
        shorebird_report_launch_start();
        shorebird_report_launch_success();
        assert_eq!(shorebird_current_boot_patch_number(), 1);

        // Phase 2: server rolls back patch 1 with no replacement.
        // Phase-1 spawned threads (PatchDownload, PatchInstallSuccess) hold a
        // clone of the config from when they were spawned, so they hit the
        // phase-1 hooks above. Phase 2 only does a patch check, so no report
        // is expected here.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: false,
                    patch: None,
                    rolled_back_patch_numbers: Some(vec![1]),
                })
            },
            UNEXPECTED_DOWNLOAD,
            UNEXPECTED_REPORT,
        );
        assert!(!shorebird_check_for_downloadable_update(std::ptr::null()));
        assert_eq!(shorebird_current_boot_patch_number(), 1);
        assert_eq!(shorebird_next_boot_patch_number(), 0);

        // Phase 3: simulate app restart by resetting config and re-initializing
        // against the same on-disk state (same tmp_dir).
        testing_reset_config();
        {
            let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
            let c_yaml = c_string("app_id: foo");
            assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
            free_c_string(c_yaml);
            free_parameters(c_params);
        }

        // The release boot has no next patch. running_patch is a
        // session-scoped global, so a fresh process starts with it None.
        // report_launch_start keeps it None because next_boot_patch is None.
        assert_eq!(shorebird_next_boot_patch_number(), 0);
        shorebird_report_launch_start();
        assert_eq!(shorebird_current_boot_patch_number(), 0);
        shorebird_report_launch_success();
        assert_eq!(shorebird_current_boot_patch_number(), 0);
    }

    /// Server rolls back patch 1, then later rolls it forward again with
    /// the same number and same hash. The pre-lifecycle code added the
    /// rolled-back number to `known_bad_patches` permanently for the
    /// release, so the rollforward was silently dropped on the device.
    /// The lifecycle's `cleanup` is state-aware: a server-driven
    /// rollback on a non-`Bad` patch forgets the patch entirely (no
    /// tombstone), leaving the number free to be reinstalled.
    /// shorebirdtech/shorebird#3728.
    #[serial]
    #[test]
    fn rollforward_after_server_rollback_reinstalls_patch() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // Phase 1: install patch 1 and report a successful launch.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_TESTS_PATCH.write_to(dest),
            |_url, _event| Ok(()),
        );
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));
        run_update_expecting(std::ptr::null(), SHOREBIRD_UPDATE_INSTALLED);
        shorebird_report_launch_start();
        shorebird_report_launch_success();
        assert_eq!(shorebird_current_boot_patch_number(), 1);

        // Phase 2: server rolls patch 1 back with no replacement.
        // Phase-1 spawned threads (PatchDownload, PatchInstallSuccess)
        // hold a clone of the config from when they were spawned, so they
        // hit the old report hook above. Nothing in phase 2 should report
        // or download — only a patch-check happens.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: false,
                    patch: None,
                    rolled_back_patch_numbers: Some(vec![1]),
                })
            },
            UNEXPECTED_DOWNLOAD,
            UNEXPECTED_REPORT,
        );
        assert!(!shorebird_check_for_downloadable_update(std::ptr::null()));
        assert_eq!(shorebird_next_boot_patch_number(), 0);

        // Phase 3: server rolls patch 1 forward — same number, same hash,
        // empty rolled_back list (the row's `is_rolled_back` flipped back
        // to false on the server). The device must accept this as a normal
        // "patch available" response and reinstall.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: Some(vec![]),
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_TESTS_PATCH.write_to(dest),
            |_url, _event| Ok(()),
        );

        // Pre-lifecycle: returns false because patch 1 sat in
        // `known_bad_patches` from phase 2's `remove_patch` call.
        // Post-lifecycle: phase 2's cleanup forgot patch 1 entirely
        // (no Bad tombstone for server-driven rollbacks), so this
        // installs cleanly.
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));
        run_update_expecting(std::ptr::null(), SHOREBIRD_UPDATE_INSTALLED);
        assert_eq!(shorebird_next_boot_patch_number(), 1);
    }

    /// Patch-to-patch rollback: device on patch 2, server rolls back to
    /// patch 1 (sends rollback signal AND a downloadable replacement).
    /// `check_for_downloadable_update` returns true (replacement available),
    /// and after `update_with_result` installs patch 1, the running session
    /// sees `current=2, next=1` — the signal Dart needs for
    /// `restartRequired`.
    #[serial]
    #[test]
    fn rollback_patch_to_patch_reports_current_and_next_distinctly() {
        testing_reset_config();
        let tmp_dir = TempDir::new().unwrap();

        let apk_path = tmp_dir.path().join("base.apk");
        write_fake_apk(
            apk_path.to_str().unwrap(),
            HELLO_TESTS_PATCH.base.as_bytes(),
        );
        let fake_libapp_path = tmp_dir.path().join("lib/arch/ignored.so");
        let c_params = parameters(&tmp_dir, fake_libapp_path.to_str().unwrap());
        let c_yaml = c_string("app_id: foo");
        assert!(shorebird_init(&c_params, FileCallbacks::new(), c_yaml));
        free_c_string(c_yaml);
        free_parameters(c_params);

        // Set up patch 2 (HELLO_PATCH_2_PATCH) as the running patch.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 2,
                        hash: HELLO_PATCH_2_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: None,
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_PATCH_2_PATCH.write_to(dest),
            |_url, _event| Ok(()),
        );
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));
        run_update_expecting(std::ptr::null(), SHOREBIRD_UPDATE_INSTALLED);
        shorebird_report_launch_start();
        shorebird_report_launch_success();
        assert_eq!(shorebird_current_boot_patch_number(), 2);

        // Server rolls back patch 2 with patch 1 (HELLO_TESTS_PATCH) as
        // replacement. The replacement is a distinct artifact from patch 2.
        testing_set_network_hooks(
            |_url, _request| {
                Ok(PatchCheckResponse {
                    patch_available: true,
                    patch: Some(crate::Patch {
                        number: 1,
                        hash: HELLO_TESTS_PATCH.hash.to_owned(),
                        download_url: "ignored".to_owned(),
                        hash_signature: None,
                    }),
                    rolled_back_patch_numbers: Some(vec![2]),
                })
            },
            |_url, dest: &Path, _resume_from: u64| HELLO_TESTS_PATCH.write_to(dest),
            |_url, _event| Ok(()),
        );
        assert!(shorebird_check_for_downloadable_update(std::ptr::null()));
        run_update_expecting(std::ptr::null(), SHOREBIRD_UPDATE_INSTALLED);

        // Running process is still on patch 2; next boot will be patch 1.
        assert_eq!(shorebird_current_boot_patch_number(), 2);
        assert_eq!(shorebird_next_boot_patch_number(), 1);
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
        let tmp_dir = TempDir::new().unwrap();

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
        let tmp_dir = TempDir::new().unwrap();

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
            UNEXPECTED_DOWNLOAD,
            |_url, _event| Ok(()),
        );
        {
            // Lock the mutex before starting the thread.
            let _lock = CALLBACK_MUTEX.lock().unwrap();
            // Start our thread, which should hang on that lock.
            shorebird_start_update_thread();
            // Wait for the thread to start.
            std::thread::sleep(std::time::Duration::from_millis(100));
            // When another update is already in progress, `update()` returns
            // `UpdateStatus::UpdateInProgress` rather than surfacing an error.
            // The in-flight update continues on its own.
            assert_eq!(
                updater::update(None).unwrap(),
                crate::UpdateStatus::UpdateInProgress
            );
        }
        // Unlock the lock, and wait for the thread to finish.
        std::thread::sleep(std::time::Duration::from_millis(100));
        // Now we should be able to call into shorebird again.
        // assert!(updater::update().is_ok());
    }
}
