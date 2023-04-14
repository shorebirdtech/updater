// This file handles translating the updater library's types into C types.

// Currently manually prefixing all functions with "shorebird_" to avoid
// name collisions with other libraries.
// cbindgen:prefix-with-name could do this for us.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::assets::{Asset, AssetOps, AssetProviderOps};
use crate::updater;

pub type OpenAssetFn = Option<unsafe extern "C" fn(name: *const c_char) -> *mut libc::c_void>;

pub type GetAssetLengthFn = Option<unsafe extern "C" fn(asset: *mut libc::c_void) -> libc::c_int>;

pub type ReadAssetFn = Option<
    unsafe extern "C" fn(
        asset: *mut libc::c_void,
        buffer: *mut libc::c_void,
        size: libc::size_t,
    ) -> libc::c_int,
>;

pub type SeekAssetFn = Option<
    unsafe extern "C" fn(
        asset: *mut libc::c_void,
        offset: libc::off_t,
        whence: libc::c_int,
    ) -> libc::off_t,
>;

pub type CloseAssetFn = Option<unsafe extern "C" fn(asset: *mut libc::c_void)>;

/// Struct containing callbacks to open, read, and close assets.
/// Passed as part of AppParameters to shorebird_init().  shorebird_init()
/// will copy the contents of this struct, so it is safe to free the memory
/// after calling shorebird_init().
#[repr(C)]
pub struct AssetProvider {
    pub open_asset: OpenAssetFn,
    pub get_asset_length: GetAssetLengthFn,
    pub read_asset: ReadAssetFn,
    pub seek_asset: SeekAssetFn,
    pub close_asset: CloseAssetFn,
}

/// Struct containing configuration parameters for the updater.
/// This struct is passed to shorebird_init().  shorebird_init() will
/// copy the contents of this struct, so it is safe to free the memory
/// after calling shorebird_init().
#[repr(C)]
pub struct AppParameters {
    /// release_version, required.  Named version of the app, off of which updates
    /// are based.  Can be either a version number or a hash.
    pub release_version: *const libc::c_char,

    /// Array of paths to the original aot library, required.  For Flutter apps
    /// these are the paths to the bundled libapp.so.  May be used for compression downloaded artifacts.
    pub original_libapp_paths: *const *const libc::c_char,

    /// Length of the original_libapp_paths array.
    pub original_libapp_paths_size: libc::c_int,

    /// A pointer to AAssetManager, required.  Used to resolve libapp.so.
    pub asset_provider: *const AssetProvider,

    /// Path to cache_dir where the updater will store downloaded artifacts.
    pub cache_dir: *const libc::c_char,
}

fn to_rust(c_string: *const libc::c_char) -> String {
    unsafe { CStr::from_ptr(c_string).to_str().unwrap() }.to_string()
}

fn to_rust_vector(c_array: *const *const libc::c_char, size: libc::c_int) -> Vec<String> {
    let mut result = Vec::new();
    for i in 0..size {
        let c_string = unsafe { *c_array.offset(i as isize) };
        result.push(to_rust(c_string));
    }
    result
}

fn app_config_from_c(c_params: *const AppParameters) -> updater::AppConfig {
    let c_params_ref = unsafe { &*c_params };

    updater::AppConfig {
        cache_dir: to_rust(c_params_ref.cache_dir),
        release_version: to_rust(c_params_ref.release_version),
        original_libapp_paths: to_rust_vector(
            c_params_ref.original_libapp_paths,
            c_params_ref.original_libapp_paths_size,
        ),
        asset_provider: updater::AssetProvider::new(Box::new(CallbackAssetProviderOps::from_c(
            c_params_ref.asset_provider,
        ))),
    }
}

/// Configures updater.  First parameter is a struct containing configuration
/// from the running app.  Second parameter is a YAML string containing
/// configuration compiled into the app.
#[no_mangle]
pub extern "C" fn shorebird_init(c_params: *const AppParameters, c_yaml: *const libc::c_char) {
    // Call init_logging() to ensure that the logger is initialized before
    // any other calls to the logger.
    updater::init_logging();

    let config = app_config_from_c(c_params);

    let yaml_string = to_rust(c_yaml);
    let result = updater::init(config, &yaml_string);
    match result {
        Ok(_) => {}
        Err(e) => {
            error!("Error initializing updater: {:?}", e);
        }
    }
}

/// Return the active patch number, or NULL if there is no active patch.
#[no_mangle]
pub extern "C" fn shorebird_active_patch_number() -> *mut c_char {
    let patch = updater::active_patch();
    match patch {
        Some(v) => {
            let c_patch = CString::new(v.number.to_string()).unwrap();
            c_patch.into_raw()
        }
        None => std::ptr::null_mut(),
    }
}

/// Return the path to the active patch for the app, or NULL if there is no
/// active patch.
#[no_mangle]
// rename to shorebird_patch_path
pub extern "C" fn shorebird_active_path() -> *mut c_char {
    let version = updater::active_patch();
    match version {
        Some(v) => {
            let c_version = CString::new(v.path).unwrap();
            c_version.into_raw()
        }
        None => std::ptr::null_mut(),
    }
}

/// Free a string returned by the updater library.
#[no_mangle]
pub extern "C" fn shorebird_free_string(c_string: *mut c_char) {
    unsafe {
        if c_string.is_null() {
            return;
        }
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

/// Report that the app failed to launch.  This will cause the updater to
/// attempt to roll back to the previous version if this version has not
/// been launched successfully before.
#[no_mangle]
pub extern "C" fn shorebird_report_failed_launch() {
    let result = updater::report_failed_launch();
    match result {
        Ok(_) => {}
        Err(e) => {
            error!("Error recording launch failure: {:?}", e);
        }
    }
}

struct CallbackAssetProviderOps {
    open: OpenAssetFn,
    seek: SeekAssetFn,
    read: ReadAssetFn,
    close: CloseAssetFn,
}

struct CallbackAssetOps {
    asset: *mut libc::c_void,
    seek: SeekAssetFn,
    read: ReadAssetFn,
    close: CloseAssetFn,
}

impl CallbackAssetProviderOps {
    fn from_c(c_asset_provider: *const AssetProvider) -> CallbackAssetProviderOps {
        info!("CallbackAssetProviderOps::from_c({:?})", c_asset_provider);
        let c_asset_provider_ref = unsafe { &*c_asset_provider };
        CallbackAssetProviderOps {
            open: c_asset_provider_ref.open_asset,
            seek: c_asset_provider_ref.seek_asset,
            read: c_asset_provider_ref.read_asset,
            close: c_asset_provider_ref.close_asset,
        }
    }
}

impl AssetProviderOps for CallbackAssetProviderOps {
    fn open(&self, path: &str) -> Option<Asset> {
        info!("CallbackAssetProviderOps::open({:?})", path);
        let c_open = self.open.unwrap();
        let c_path = CString::new(path).unwrap();
        let asset = unsafe { c_open(c_path.as_ptr()) };
        if asset.is_null() {
            info!("CallbackAssetProviderOps::open({:?}) -> None", path);
            return None;
        }
        Some(Asset::new(Box::new(CallbackAssetOps {
            asset: asset,
            seek: self.seek,
            read: self.read,
            close: self.close,
        })))
    }
}

impl AssetOps for CallbackAssetOps {
    fn close(&mut self) {
        let c_close = self.close.unwrap();
        unsafe { c_close(self.asset) };
    }
}

impl std::fmt::Debug for CallbackAssetOps {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        write!(f, "CallbackAssetOps({:?})", self.asset)
    }
}

impl std::io::Read for CallbackAssetOps {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        info!("read({:?}, {:?})", self, buf.len());
        let c_read = self.read.unwrap();
        let result =
            unsafe { c_read(self.asset, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if result < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Error reading asset",
            ));
        }
        Ok(result as usize)
    }
}

impl std::io::Seek for CallbackAssetOps {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        info!("seek({:?}, {:?})", self, pos);
        let c_seek = self.seek.unwrap();

        let result = match pos {
            std::io::SeekFrom::Start(v) => unsafe { c_seek(self.asset, v as i64, 0) },
            std::io::SeekFrom::End(v) => unsafe { c_seek(self.asset, v, 2) },
            std::io::SeekFrom::Current(v) => unsafe { c_seek(self.asset, v, 1) },
        };
        if result < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Error seeking asset",
            ));
        }
        Ok(result as u64)
    }
}
