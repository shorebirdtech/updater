// This file's job is to deal with the update_server and network side
// of the updater library.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::string::ToString;

use crate::cache::UpdaterState;
use crate::config::{current_arch, current_platform, UpdateConfig};
use crate::models::{PatchCheckRequest, PatchCheckResponse};

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::println as debug; // Workaround to use println! for logs.

fn patches_check_url(base_url: &str) -> String {
    return format!("{}/api/v1/patches/check", base_url);
}

pub type PatchCheckRequestFn = fn(&str, PatchCheckRequest) -> anyhow::Result<PatchCheckResponse>;
pub type DownloadFileFn = fn(&str) -> anyhow::Result<Vec<u8>>;

/// A container for network callbacks which can be mocked out for testing.
#[derive(Clone)]
pub struct NetworkHooks {
    /// The function to call to send a patch check request.
    pub patch_check_request_fn: PatchCheckRequestFn,
    /// The function to call to download a file.
    pub download_file_fn: DownloadFileFn,
}

// We have to implement Debug by hand since fn types don't implement it.
impl core::fmt::Debug for NetworkHooks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkHooks")
            .field("patch_check_request_fn", &"<fn>")
            .field("download_file_fn", &"<fn>")
            .finish()
    }
}

#[cfg(test)]
fn patch_check_request_throws(
    _url: &str,
    _request: PatchCheckRequest,
) -> anyhow::Result<PatchCheckResponse> {
    anyhow::bail!("please set a patch_check_request_fn");
}

#[cfg(test)]
fn download_file_throws(_url: &str) -> anyhow::Result<Vec<u8>> {
    anyhow::bail!("please set a download_file_fn");
}

impl Default for NetworkHooks {
    #[cfg(not(test))]
    fn default() -> Self {
        Self {
            patch_check_request_fn: patch_check_request_default,
            download_file_fn: download_file_default,
        }
    }

    #[cfg(test)]
    fn default() -> Self {
        Self {
            patch_check_request_fn: patch_check_request_throws,
            download_file_fn: download_file_throws,
        }
    }
}

#[cfg(not(test))]
pub fn patch_check_request_default(
    url: &str,
    request: PatchCheckRequest,
) -> anyhow::Result<PatchCheckResponse> {
    use std::error::Error;

    let client = reqwest::blocking::Client::new();
    match client.post(url).json(&request).send() {
        Ok(response) => {
            let response = response.json()?;
            Ok(response)
        }
        Err(e) => match e.source() {
            Some(source)
                if source
                    .to_string()
                    .contains("failed to lookup address information") =>
            {
                anyhow::bail!("Patch check request failed due to network error. Please check your internet connection.");
            }
            _ => Err(e.into()),
        },
    }
}

#[cfg(not(test))]
pub fn download_file_default(url: &str) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::new();
    let response = client.get(url).send()?;
    let bytes = response.bytes()?;
    // Patch files are small (e.g. 50kb) so this should be ok to copy into memory.
    Ok(bytes.to_vec())
}

#[cfg(test)]
/// Unit tests can call this to mock out the network calls.
pub fn testing_set_network_hooks(
    patch_check_request_fn: PatchCheckRequestFn,
    download_file_fn: DownloadFileFn,
) {
    crate::config::with_config_mut(|maybe_config| match maybe_config {
        Some(config) => {
            config.network_hooks = NetworkHooks {
                patch_check_request_fn,
                download_file_fn,
            };
        }
        None => {
            panic!("testing_set_network_hooks called before config was initialized");
        }
    });
}

pub fn send_patch_check_request(
    config: &UpdateConfig,
    state: &UpdaterState,
) -> anyhow::Result<PatchCheckResponse> {
    let latest_patch_number = state.latest_patch_number();

    // Send the request to the server.
    let request = PatchCheckRequest {
        app_id: config.app_id.clone(),
        channel: config.channel.clone(),
        release_version: config.release_version.clone(),
        patch_number: latest_patch_number,
        platform: current_platform().to_string(),
        arch: current_arch().to_string(),
    };
    debug!("Sending patch check request: {:?}", request);
    let url = &patches_check_url(&config.base_url);
    let patch_check_request_fn = config.network_hooks.patch_check_request_fn;
    let response = patch_check_request_fn(url, request)?;

    debug!("Patch check response: {:?}", response);
    return Ok(response);
}

pub fn download_to_path(
    network_hooks: &NetworkHooks,
    url: &str,
    path: &Path,
) -> anyhow::Result<()> {
    debug!("Downloading patch from: {}", url);
    // Download the file at the given url to the given path.
    let download_file_hook = network_hooks.download_file_fn;
    let mut bytes = download_file_hook(url)?;
    // Ensure the download directory exists.
    if let Some(parent) = path.parent() {
        debug!("Creating download directory: {:?}", parent);
        std::fs::create_dir_all(parent)?;
    }

    debug!("Writing download to: {:?}", path);
    let mut file = File::create(path)?;
    file.write_all(&mut bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    // This confirms that the default network hooks throw an error in cfg(test).
    // In cfg(not(test)) they should be set to the default implementation
    // which makes real network calls.
    #[test]
    fn default_network_hooks_throws() {
        let network_hooks = super::NetworkHooks::default();
        let result = (network_hooks.patch_check_request_fn)(
            "",
            super::PatchCheckRequest {
                app_id: "".to_string(),
                channel: "".to_string(),
                release_version: "".to_string(),
                patch_number: None,
                platform: "".to_string(),
                arch: "".to_string(),
            },
        );
        assert!(result.is_err());
        let result = (network_hooks.download_file_fn)("");
        assert!(result.is_err());
    }

    #[test]
    fn network_hooks_debug() {
        let network_hooks = super::NetworkHooks::default();
        let debug = format!("{:?}", network_hooks);
        assert!(debug.contains("patch_check_request_fn"));
        assert!(debug.contains("download_file_fn"));
    }
}
