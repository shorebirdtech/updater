// This file handles the global config for the updater library.
use anyhow::bail;

#[cfg(test)]
use crate::network::{DownloadFileFn, PatchCheckRequestFn};
#[cfg(test)]
use std::cell::RefCell;

use crate::updater::AppConfig;
use crate::yaml::YamlConfig;
use crate::UpdateError;

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
fn global_config() -> &'static Mutex<ResolvedConfig> {
    static INSTANCE: OnceCell<Mutex<ResolvedConfig>> = OnceCell::new();
    INSTANCE.get_or_init(|| Mutex::new(ResolvedConfig::empty()))
}

#[cfg(test)]
pub struct ThreadConfig {
    resolved_config: ResolvedConfig,
    pub patch_check_request_fn: Option<PatchCheckRequestFn>,
    pub download_file_fn: Option<DownloadFileFn>,
}

#[cfg(test)]
impl ThreadConfig {
    fn empty() -> Self {
        Self {
            resolved_config: ResolvedConfig::empty(),
            patch_check_request_fn: None,
            download_file_fn: None,
        }
    }
}

#[cfg(test)]
thread_local!(static THREAD_CONFIG: RefCell<ThreadConfig> = RefCell::new(ThreadConfig::empty()));

#[cfg(test)]
/// Unit tests should call this to reset the config between tests.
pub fn testing_reset_config() {
    THREAD_CONFIG.with(|thread_config| {
        let mut thread_config = thread_config.borrow_mut();
        *thread_config = ThreadConfig::empty();
    });
}

pub fn check_initialized_and_call<F, R>(f: F, config: &ResolvedConfig) -> anyhow::Result<R>
where
    F: FnOnce(&ResolvedConfig) -> anyhow::Result<R>,
{
    if !config.is_initialized {
        bail!(UpdateError::ConfigNotInitialized);
    }
    return f(&config);
}

#[cfg(test)]
pub fn with_thread_config<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&ThreadConfig) -> anyhow::Result<R>,
{
    THREAD_CONFIG.with(|thread_config| {
        let thread_config = thread_config.borrow();
        f(&thread_config)
    })
}

#[cfg(test)]
pub fn with_thread_config_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut ThreadConfig) -> R,
{
    THREAD_CONFIG.with(|thread_config| {
        let mut thread_config = thread_config.borrow_mut();
        f(&mut thread_config)
    })
}

#[cfg(not(test))]
pub fn with_config<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&ResolvedConfig) -> anyhow::Result<R>,
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
    F: FnOnce(&ResolvedConfig) -> anyhow::Result<R>,
{
    // Rust unit tests run on multiple threads in parallel, so we use
    // a per-thread config when unit testing instead of a global config.
    // The global config code paths are covered by the integration tests.
    THREAD_CONFIG.with(|thread_config| {
        let thread_config = thread_config.borrow();
        check_initialized_and_call(f, &thread_config.resolved_config)
    })
}

#[cfg(not(test))]
pub fn with_config_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut ResolvedConfig) -> R,
{
    let mut lock = global_config()
        .lock()
        .expect("Failed to acquire updater lock.");
    f(&mut lock)
}

#[cfg(test)]
pub fn with_config_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut ResolvedConfig) -> R,
{
    THREAD_CONFIG.with(|thread_config| {
        let mut thread_config = thread_config.borrow_mut();
        f(&mut thread_config.resolved_config)
    })
}

#[derive(Debug)]
pub struct ResolvedConfig {
    // is_initialized could be Option<ResolvedConfig> with some refactoring.
    is_initialized: bool,
    pub cache_dir: String,
    pub download_dir: String,
    pub channel: String,
    pub app_id: String,
    pub release_version: String,
    pub original_libapp_paths: Vec<String>,
    pub base_url: String,
}

impl ResolvedConfig {
    pub fn empty() -> Self {
        Self {
            is_initialized: false,
            cache_dir: String::new(),
            download_dir: String::new(),
            channel: String::new(),
            app_id: String::new(),
            release_version: String::new(),
            original_libapp_paths: Vec::new(),
            base_url: String::new(),
        }
    }
}

pub fn set_config(app_config: AppConfig, yaml: YamlConfig) -> anyhow::Result<()> {
    with_config_mut(|config| {
        // Tests currently call set_config() multiple times, so we can't check
        // this yet.
        // anyhow::ensure!(!lock.is_initialized, "Updater config can only be set once.");

        config.base_url = yaml
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
            .to_owned();
        config.channel = yaml
            .channel
            .as_deref()
            .unwrap_or(DEFAULT_CHANNEL)
            .to_owned();
        config.cache_dir = app_config.cache_dir.to_string();
        let mut cache_path = std::path::PathBuf::from(app_config.cache_dir);
        cache_path.push("downloads");
        config.download_dir = cache_path.to_str().unwrap().to_string();
        config.app_id = yaml.app_id.to_string();
        config.release_version = app_config.release_version.to_string();
        config.original_libapp_paths = app_config.original_libapp_paths;
        config.is_initialized = true;
        info!("Updater configured with: {:?}", config);
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
    return PLATFORM;
}
