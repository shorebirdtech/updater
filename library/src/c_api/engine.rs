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
use crate::restart;
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

/// One HTTP header: a NUL-terminated name and value.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct HttpHeader {
    pub name: *const libc::c_char,
    pub value: *const libc::c_char,
}

/// An HTTP request the updater asks the engine to send.
///
/// Every pointer is owned by the updater and valid only for the duration of
/// the `EngineCallbacks::http_request` call.
#[repr(C)]
pub struct HttpRequest {
    /// "GET" or "POST".
    pub method: *const libc::c_char,
    /// An absolute `http://` or `https://` URL.
    pub url: *const libc::c_char,
    pub headers: *const HttpHeader,
    pub header_count: usize,
    /// The request body, or null with `body_length` 0 for none.
    pub body: *const u8,
    pub body_length: usize,
    /// How long the whole request may take, in milliseconds, before it fails
    /// with `SHOREBIRD_HTTP_TRANSPORT_ERROR`; 0 for no limit beyond the
    /// client's own.
    pub timeout_ms: u32,
}

/// Where the engine delivers a response, on the thread that called
/// `EngineCallbacks::http_request`, before that call returns.
#[repr(C)]
pub struct HttpResponseCallbacks {
    /// Passed back unchanged as the first argument of each callback.
    pub context: *mut libc::c_void,

    /// Called once with the status and headers of the final response (after
    /// any redirects), before any body bytes; a second call fails the request. The pointers are valid only
    /// during the call. Returning false stops the request, which then returns
    /// `SHOREBIRD_HTTP_ABORTED`.
    pub on_response: extern "C" fn(
        context: *mut libc::c_void,
        status: u16,
        headers: *const HttpHeader,
        header_count: usize,
    ) -> bool,

    /// Called with each run of body bytes, in order, exactly as received: the
    /// updater sends `Accept-Encoding: identity`, and the client must not
    /// decode a `Content-Encoding` (the updater checks the bytes against
    /// `Content-Length`). Returning false stops the request, which then
    /// returns `SHOREBIRD_HTTP_ABORTED`.
    pub on_body: extern "C" fn(context: *mut libc::c_void, data: *const u8, length: usize) -> bool,

    /// May be called once, before `http_request` returns a code other than
    /// `SHOREBIRD_HTTP_OK` or `SHOREBIRD_HTTP_ABORTED`, with why the request
    /// failed (the platform error's description), for logs and the failure
    /// event. `message` is NUL-terminated and valid only during the call.
    pub on_error: extern "C" fn(context: *mut libc::c_void, message: *const libc::c_char),
}

/// The whole response was delivered.
pub const SHOREBIRD_HTTP_OK: i32 = 0;
/// The server could not be reached or the connection failed: DNS, connect,
/// a read or write error, or a timeout.
pub const SHOREBIRD_HTTP_TRANSPORT_ERROR: i32 = 1;
/// The TLS handshake failed, including an untrusted certificate.
pub const SHOREBIRD_HTTP_TLS_ERROR: i32 = 2;
/// The server's response was not valid HTTP/1.1, or redirected too often.
pub const SHOREBIRD_HTTP_PROTOCOL_ERROR: i32 = 3;
/// A response callback returned false.
pub const SHOREBIRD_HTTP_ABORTED: i32 = 4;
/// The request itself was unusable, e.g. a malformed URL.
pub const SHOREBIRD_HTTP_INVALID_REQUEST: i32 = 5;

/// Services the engine provides to the updater: a signature verifier on the
/// engine's BoringSSL, and an HTTP client on the platform's HTTP stack.
///
/// The updater uses `http_request` only on iOS, macOS and Android; on other
/// platforms it sends requests itself and ignores it.
///
/// Either function may be null when the engine does not provide it: with no
/// `http_request`, network requests fail as if the device were offline; with
/// no `verify_signature`, `shorebird_init` fails for an app configured with a
/// patch public key.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct EngineCallbacks {
    /// Sends `request` and delivers the response through `callbacks`,
    /// blocking until done. Returns one of the `SHOREBIRD_HTTP_*` codes. A
    /// response with any status, including 4xx and 5xx, is
    /// `SHOREBIRD_HTTP_OK` as long as it was delivered. Must be safe to call
    /// from any thread.
    pub http_request: Option<
        extern "C" fn(request: *const HttpRequest, callbacks: *const HttpResponseCallbacks) -> i32,
    >,
    /// Returns whether `signature` is a valid RSASSA-PKCS1-v1_5 SHA-256
    /// signature of `message` by `public_key`, a DER-encoded PKCS#1
    /// RSAPublicKey with a 2048 to 8192 bit modulus. Must be safe to call
    /// from any thread.
    pub verify_signature: Option<
        extern "C" fn(
            public_key: *const u8,
            public_key_length: usize,
            message: *const u8,
            message_length: usize,
            signature: *const u8,
            signature_length: usize,
        ) -> bool,
    >,
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

/// Registers the engine's network and signature services. Call before
/// `shorebird_init`: until this is called, network requests on iOS, macOS and
/// Android fail as if the device were offline, and `shorebird_init` fails for
/// an app configured with a patch public key. A later call replaces the
/// earlier callbacks.
///
/// # Safety
///
/// `callbacks` must be null (ignored) or point to a valid `EngineCallbacks`.
/// The updater copies the struct; the functions it names must stay callable
/// for the life of the process.
#[no_mangle]
pub unsafe extern "C" fn shorebird_set_engine_callbacks(callbacks: *const EngineCallbacks) {
    if callbacks.is_null() {
        shorebird_error!("Null callbacks passed to shorebird_set_engine_callbacks");
        return;
    }
    crate::engine_callbacks::set(unsafe { *callbacks });
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
    // The thread runs detached; nothing waits for it.
    drop(updater::start_update_thread());
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

/// Report that a hot restart could not bring up the patch it was
/// relaunching into: the isolate snapshot would not resolve, the patch
/// would not load, or the relaunch itself failed.
///
/// Use this rather than `shorebird_report_launch_failure` for a failure
/// of the restart mechanism. It rolls the boot target back so the
/// recovery relaunch lands on the last patch that booted, but leaves the
/// patch installed and un-tombstoned, because a restart can fail for
/// reasons the patch has no part in and a cold boot onto it is how we
/// find out. The exclusion lasts for this process only.
#[no_mangle]
pub extern "C" fn shorebird_report_restart_failure() {
    log_on_error(
        updater::report_restart_failure,
        "reporting restart failure",
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

/// Registers the engine's restart handler, used to service
/// `shorebird_restart_app` requests from `package:shorebird_code_push`.
/// The handler must tear down and relaunch the app's Dart isolate, loading
/// the current next-boot patch, and return true if the restart was
/// scheduled. Pass null to unregister (e.g. if the last engine capable of
/// restarting is destroyed).
#[no_mangle]
pub extern "C" fn shorebird_set_restart_handler(handler: Option<extern "C" fn() -> bool>) {
    restart::set_restart_handler(handler);
}
