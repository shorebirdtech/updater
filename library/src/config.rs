// This file handles the global config for the updater library.
#[cfg(test)]
use crate::network::{DownloadFileFn, PatchCheckRequestFn};
#[cfg(test)]
use std::cell::RefCell;

use crate::updater::AppConfig;
use crate::yaml::YamlConfig;
use crate::UpdateError;
use anyhow::Context;
use std::path::{Path, PathBuf};

#[cfg(not(test))]
use once_cell::sync::OnceCell;
#[cfg(not(test))]
use std::sync::Mutex;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::println as info; // Workaround to use println! for logs.

// cbindgen looks for const, ignore these so it doesn't warn about them.

/// cbindgen:ignore
const DEFAULT_BASE_URL: &'static str = "https://api.shorebird.dev";
/// cbindgen:ignore
const DEFAULT_CHANNEL: &'static str = "stable";

#[cfg(not(test))]
fn global_config() -> &'static Mutex<Option<UpdateConfig>> {
    static INSTANCE: OnceCell<Mutex<Option<UpdateConfig>>> = OnceCell::new();
    INSTANCE.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
thread_local!(static THREAD_CONFIG: RefCell<Option<UpdateConfig>> = RefCell::new(None));

#[cfg(test)]
/// Unit tests should call this to reset the config between tests.
pub fn testing_reset_config() {
    THREAD_CONFIG.with(|thread_config| {
        let mut thread_config = thread_config.borrow_mut();
        *thread_config = None;
    });
}

pub fn check_initialized_and_call<F, R>(
    f: F,
    maybe_config: &Option<UpdateConfig>,
) -> anyhow::Result<R>
where
    F: FnOnce(&UpdateConfig) -> anyhow::Result<R>,
{
    match maybe_config {
        Some(config) => f(&config),
        None => anyhow::bail!(UpdateError::ConfigNotInitialized),
    }
}

#[cfg(not(test))]
pub fn with_config<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&UpdateConfig) -> anyhow::Result<R>,
{
    // expect() here should be OK, it's job is to propagate a panic across
    // threads if the lock is poisoned.
    let lock = global_config()
        .lock()
        .expect("Failed to acquire updater lock.");
    check_initialized_and_call(f, &lock)
}

#[cfg(test)]
pub fn with_config<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&UpdateConfig) -> anyhow::Result<R>,
{
    // Rust unit tests run on multiple threads in parallel, so we use
    // a per-thread config when unit testing instead of a global config.
    // The global config code paths are covered by the integration tests.
    THREAD_CONFIG.with(|thread_config| {
        let thread_config = thread_config.borrow();
        check_initialized_and_call(f, &thread_config)
    })
}

#[cfg(not(test))]
pub fn with_config_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut Option<UpdateConfig>) -> R,
{
    let mut lock = global_config()
        .lock()
        .expect("Failed to acquire updater lock.");
    f(&mut lock)
}

#[cfg(test)]
pub fn with_config_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut Option<UpdateConfig>) -> R,
{
    THREAD_CONFIG.with(|thread_config| {
        let mut thread_config = thread_config.borrow_mut();
        f(&mut thread_config)
    })
}

// The config passed into init.  This is immutable once set and copyable.
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    pub cache_dir: PathBuf,
    pub download_dir: PathBuf,
    pub channel: String,
    pub app_id: String,
    pub release_version: String,
    pub libapp_path: PathBuf,
    pub base_url: String,
}

pub fn set_config(
    app_config: AppConfig,
    libapp_path: PathBuf,
    yaml: YamlConfig,
) -> anyhow::Result<()> {
    with_config_mut(|config| {
        // Tests currently call set_config() multiple times, so we can't check
        // this yet.
        // anyhow::ensure!(!lock.is_initialized, "Updater config can only be set once.");

        let mut cache_path = std::path::PathBuf::from(app_config.cache_dir);
        cache_path.push("downloads");
        let download_dir = cache_path;

        let new_config = UpdateConfig {
            cache_dir: std::path::PathBuf::from(app_config.cache_dir),
            download_dir: download_dir,
            channel: yaml
                .channel
                .as_deref()
                .unwrap_or(DEFAULT_CHANNEL)
                .to_owned(),
            app_id: yaml.app_id.to_string(),
            release_version: app_config.release_version.to_string(),
            libapp_path,
            base_url: yaml
                .base_url
                .as_deref()
                .unwrap_or(DEFAULT_BASE_URL)
                .to_owned(),
        };
        info!("Updater configured with: {:?}", config);
        *config = Some(new_config);

        Ok(())
    })
}

// Arch/Platform names need to be kept in sync with the shorebird cli.
pub fn current_arch() -> &'static str {
    #[cfg(target_arch = "x86")]
    static ARCH: &str = "x86";
    #[cfg(target_arch = "x86_64")]
    static ARCH: &str = "x86_64";
    #[cfg(target_arch = "aarch64")]
    static ARCH: &str = "aarch64";
    #[cfg(target_arch = "arm")]
    static ARCH: &str = "arm";
    return ARCH;
}

pub fn current_platform() -> &'static str {
    #[cfg(target_os = "macos")]
    static PLATFORM: &str = "macos";
    #[cfg(target_os = "linux")]
    static PLATFORM: &str = "linux";
    #[cfg(target_os = "windows")]
    static PLATFORM: &str = "windows";
    #[cfg(target_os = "android")]
    static PLATFORM: &str = "android";
    #[cfg(target_os = "ios")]
    static PLATFORM: &str = "ios";
    return PLATFORM;
}
