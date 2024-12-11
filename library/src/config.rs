// This file handles the global config for the updater library.
use crate::network::NetworkHooks;

use crate::updater::AppConfig;
use crate::yaml::YamlConfig;
use crate::{ExternalFileProvider, UpdateError};
use std::path::PathBuf;

use anyhow::{bail, Result};
use once_cell::sync::OnceCell;
use std::sync::Mutex;

// cbindgen looks for const, ignore these so it doesn't warn about them.

/// cbindgen:ignore
#[cfg(test)]
const DEFAULT_BASE_URL: &str = "DEFAULT_BASE_URL should be mocked using mockito::Server";

/// cbindgen:ignore
#[cfg(not(test))]
const DEFAULT_BASE_URL: &str = "https://api.shorebird.dev";

/// cbindgen:ignore
const DEFAULT_CHANNEL: &str = "stable";

fn global_config() -> &'static Mutex<Option<UpdateConfig>> {
    static INSTANCE: OnceCell<Mutex<Option<UpdateConfig>>> = OnceCell::new();
    INSTANCE.get_or_init(|| Mutex::new(None))
}

/// Unit tests should call this to reset the config between tests.
#[cfg(test)]
pub fn testing_reset_config() {
    with_config_mut(|config| {
        *config = None;
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
        Some(config) => f(config),
        None => anyhow::bail!(UpdateError::ConfigNotInitialized),
    }
}

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

pub fn with_config_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut Option<UpdateConfig>) -> R,
{
    let mut lock = global_config()
        .lock()
        .expect("Failed to acquire updater lock.");
    f(&mut lock)
}

// The config passed into init.  This is immutable once set and copyable.
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    pub storage_dir: PathBuf,
    pub download_dir: PathBuf,
    pub auto_update: bool,
    pub channel: String,
    pub app_id: String,
    pub release_version: String,
    pub libapp_path: PathBuf,
    pub base_url: String,
    pub network_hooks: NetworkHooks,
    pub file_provider: Box<dyn ExternalFileProvider>,
    pub patch_public_key: Option<String>,
}

/// Returns Ok if the config was set successfully, Err if it was already set.
pub fn set_config(
    app_config: AppConfig,
    file_provider: Box<dyn ExternalFileProvider>,
    libapp_path: PathBuf,
    yaml: &YamlConfig,
    network_hooks: NetworkHooks,
) -> Result<()> {
    with_config_mut(|config: &mut Option<UpdateConfig>| {
        if config.is_some() {
            // This previously returned an error, but this happens regularly
            // with apps that use Firebase Messaging, and logging it as an error
            // has caused confusion.
            bail!("Updater already initialized, ignoring second shorebird_init call.");
        }

        let mut code_cache_path = std::path::PathBuf::from(&app_config.code_cache_dir);
        code_cache_path.push("downloads");
        let download_dir = code_cache_path;

        let new_config = UpdateConfig {
            storage_dir: std::path::PathBuf::from(app_config.app_storage_dir),
            download_dir,
            channel: yaml
                .channel
                .as_deref()
                .unwrap_or(DEFAULT_CHANNEL)
                .to_owned(),
            auto_update: yaml.auto_update.unwrap_or(true),
            app_id: yaml.app_id.to_string(),
            release_version: app_config.release_version.to_string(),
            libapp_path,
            base_url: yaml
                .base_url
                .as_deref()
                .unwrap_or(DEFAULT_BASE_URL)
                .to_owned(),
            network_hooks,
            file_provider,
            patch_public_key: yaml.patch_public_key.to_owned(),
        };
        shorebird_debug!("Updater configured with: {:?}", new_config);
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
    ARCH
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
    PLATFORM
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::set_config;
    use crate::{network::NetworkHooks, testing_reset_config, AppConfig, ExternalFileProvider};
    use anyhow::Result;
    use serial_test::serial;

    #[derive(Debug, Clone)]
    pub struct FakeExternalFileProvider {}
    impl ExternalFileProvider for FakeExternalFileProvider {
        fn open(&self) -> anyhow::Result<Box<dyn crate::ReadSeek>> {
            Ok(Box::new(std::io::Cursor::new(vec![])))
        }
    }

    fn fake_app_config() -> AppConfig {
        AppConfig {
            app_storage_dir: "/tmp".to_string(),
            code_cache_dir: "/tmp".to_string(),
            release_version: "1.0.0".to_string(),
            original_libapp_paths: vec!["libapp.so".to_string()],
        }
    }

    fn fake_yaml() -> crate::yaml::YamlConfig {
        crate::yaml::YamlConfig {
            app_id: "fake_app_id".to_string(),
            channel: Some("fake_channel".to_string()),
            auto_update: Some(true),
            base_url: Some("fake_base_url".to_string()),
            patch_public_key: None,
        }
    }

    // These tests are serial because they modify global state.
    #[serial]
    #[test]
    fn set_config_correctly_sets_values() -> Result<()> {
        testing_reset_config();

        set_config(
            AppConfig {
                app_storage_dir: "/app_storage".to_string(),
                code_cache_dir: "/code_cache".to_string(),
                release_version: "1.0.0".to_string(),
                original_libapp_paths: vec!["libapp.so".to_string()],
            },
            Box::new(FakeExternalFileProvider {}),
            "first_path".into(),
            &crate::yaml::YamlConfig {
                app_id: "fake_app_id".to_string(),
                channel: Some("fake_channel".to_string()),
                auto_update: Some(true),
                base_url: Some("fake_base_url".to_string()),
                patch_public_key: Some("patch_public_key".to_string()),
            },
            NetworkHooks::default(),
        )?;

        let config = super::with_config(|config| Ok(config.clone())).unwrap();
        assert_eq!(config.storage_dir, PathBuf::from("/app_storage"));
        assert_eq!(
            config.download_dir,
            PathBuf::from("/").join("code_cache").join("downloads")
        );
        assert!(config.auto_update);
        assert_eq!(config.channel, "fake_channel");
        assert_eq!(config.app_id, "fake_app_id");
        assert_eq!(config.release_version, "1.0.0");
        assert_eq!(config.libapp_path.to_str(), Some("first_path"));
        assert_eq!(config.base_url, "fake_base_url");
        // We should also validate network hooks here
        assert_eq!(
            config.patch_public_key,
            Some("patch_public_key".to_string())
        );

        Ok(())
    }

    // These tests are serial because they modify global state.
    #[serial]
    #[test]
    fn set_config_returns_err_on_subsequent_calls() -> Result<()> {
        testing_reset_config();

        assert!(set_config(
            fake_app_config(),
            Box::new(FakeExternalFileProvider {}),
            "first_path".into(),
            &fake_yaml(),
            NetworkHooks::default(),
        )
        .is_ok());

        assert!(set_config(
            fake_app_config(),
            Box::new(FakeExternalFileProvider {}),
            "second_path".into(),
            &fake_yaml(),
            NetworkHooks::default(),
        )
        .is_err());

        let config = super::with_config(|config| Ok(config.clone())).unwrap();
        assert_eq!(config.libapp_path.to_str(), Some("first_path"));

        Ok(())
    }
}
