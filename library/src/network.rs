// This file's job is to deal with the update_server and network side
// of the updater library.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::string::ToString;

use crate::cache::UpdaterState;
use crate::config::{current_arch, current_platform, UpdateConfig};

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::println as debug; // Workaround to use println! for logs.

fn patches_check_url(base_url: &str) -> String {
    return format!("{}/api/v1/patches/check", base_url);
}

fn patches_events_url(base_url: &str) -> String {
    return format!("{}/api/v1/patches/events", base_url);
}

pub type PatchCheckRequestFn = fn(&str, PatchCheckRequest) -> anyhow::Result<PatchCheckResponse>;
pub type DownloadFileFn = fn(&str) -> anyhow::Result<Vec<u8>>;
pub type PatchInstallSuccessFn = fn(&str, CreatePatchInstallEventRequest) -> anyhow::Result<()>;

/// A container for network clalbacks which can be mocked out for testing.
#[derive(Clone)]
pub struct NetworkHooks {
    /// The function to call to send a patch check request.
    pub patch_check_request_fn: PatchCheckRequestFn,
    /// The function to call to download a file.
    pub download_file_fn: DownloadFileFn,
    /// The function to call to report patch install success.
    pub patch_install_success_fn: PatchInstallSuccessFn,
}

// We have to implement Debug by hand since fn types don't implement it.
impl core::fmt::Debug for NetworkHooks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkHooks")
            .field("patch_check_request_fn", &"<fn>")
            .field("download_file_fn", &"<fn>")
            .field("patch_install_success_fn", &"<fn>")
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

#[cfg(test)]
pub fn patch_install_success_throws(
    _url: &str,
    _request: CreatePatchInstallEventRequest,
) -> anyhow::Result<()> {
    anyhow::bail!("please set a patch_install_success_fn");
}

impl Default for NetworkHooks {
    #[cfg(not(test))]
    fn default() -> Self {
        Self {
            patch_check_request_fn: patch_check_request_default,
            download_file_fn: download_file_default,
            patch_install_success_fn: patch_install_success_default,
        }
    }

    #[cfg(test)]
    fn default() -> Self {
        Self {
            patch_check_request_fn: patch_check_request_throws,
            download_file_fn: download_file_throws,
            patch_install_success_fn: patch_install_success_throws,
        }
    }
}

#[cfg(not(test))]
pub fn patch_check_request_default(
    url: &str,
    request: PatchCheckRequest,
) -> anyhow::Result<PatchCheckResponse> {
    let client = reqwest::blocking::Client::new();
    let response = client.post(url).json(&request).send()?.json()?;
    Ok(response)
}

#[cfg(not(test))]
pub fn download_file_default(url: &str) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::new();
    let response = client.get(url).send()?;
    let bytes = response.bytes()?;
    // Patch files are small (e.g. 50kb) so this should be ok to copy into memory.
    Ok(bytes.to_vec())
}

#[cfg(not(test))]
pub fn patch_install_success_default(
    url: &str,
    request: CreatePatchInstallEventRequest,
) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::new();
    let _ = client.post(url).json(&request).send()?;
    Ok(())
}

#[cfg(test)]
/// Unit tests can call this to mock out the network calls.
pub fn testing_set_network_hooks(
    patch_check_request_fn: PatchCheckRequestFn,
    download_file_fn: DownloadFileFn,
    patch_install_success_fn: PatchInstallSuccessFn,
) {
    crate::config::with_config_mut(|maybe_config| match maybe_config {
        Some(config) => {
            config.network_hooks = NetworkHooks {
                patch_check_request_fn,
                download_file_fn,
                patch_install_success_fn,
            };
        }
        None => {
            panic!("testing_set_network_hooks called before config was initialized");
        }
    });
}

#[derive(Debug, Deserialize)]
pub struct Patch {
    /// The patch number.  Starts at 1 for each new release and increases
    /// monotonically.
    pub number: usize,
    /// The hex-encoded sha256 hash of the final uncompressed patch file.
    /// Legacy: originally "#" before we implemented hash checks (remove).
    pub hash: String,
    /// The URL to download the patch file from.
    pub download_url: String,
}

#[derive(Debug, Serialize)]
pub struct PatchCheckRequest {
    /// The Shorebird app_id built into the shorebird.yaml in the app.
    pub app_id: String,
    /// The Shorebird channel built into the shorebird.yaml in the app.
    pub channel: String,
    /// The release version from AndroidManifest.xml, Info.plist in the app.
    pub release_version: String,
    /// The latest patch number that the client has downloaded.
    /// Not necessarily the one it's running (if some have been marked bad).
    /// We could rename this to be more clear.    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_number: Option<usize>,
    /// Platform (e.g. "android", "ios", "windows", "macos", "linux").
    pub platform: String,
    /// Architecture we're running (e.g. "aarch64", "x86", "x86_64").
    pub arch: String,
}

/// An event that is sent to the server when a patch is successfully installed.
#[derive(Debug, Serialize)]
pub struct PatchInstallEvent {
    /// The Shorebird app_id built into the shorebird.yaml in the app.
    pub app_id: String,

    /// The architecture we're running (e.g. "aarch64", "x86", "x86_64").
    pub arch: String,

    /// The unique ID of this device.
    pub client_id: String,

    /// The identifier of this event.
    #[serde(rename = "type")]
    pub identifier: String,

    /// The patch number that was installed.
    pub patch_number: usize,

    /// The platform we're running on (e.g. "android", "ios", "windows", "macos", "linux").
    pub platform: String,

    /// The release version from AndroidManifest.xml, Info.plist in the app.
    pub release_version: String,
}

impl PatchInstallEvent {
    pub fn new(
        app_id: String,
        arch: String,
        client_id: String,
        patch_number: usize,
        platform: String,
        release_version: String,
    ) -> Self {
        Self {
            app_id,
            arch,
            client_id,
            identifier: "__patch_install__".to_string(),
            patch_number,
            platform,
            release_version,
        }
    }
}

/// The request body for the create patch install event endpoint.
///
/// We may want to consider making this more generic if/when we add more events
/// using something like https://github.com/dtolnay/typetag.
#[derive(Debug, Serialize)]
pub struct CreatePatchInstallEventRequest {
    event: PatchInstallEvent,
}

#[derive(Debug, Deserialize)]
pub struct PatchCheckResponse {
    pub patch_available: bool,
    #[serde(default)]
    pub patch: Option<Patch>,
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

pub fn report_successful_patch_install(
    config: &UpdateConfig,
    state: &UpdaterState,
    patch_number: usize,
) -> anyhow::Result<()> {
    let client_id = state
        .client_id
        .clone()
        .unwrap_or("".to_string())
        .to_string();

    let event = PatchInstallEvent::new(
        config.app_id.clone(),
        current_arch().to_string(),
        client_id.to_string(),
        patch_number,
        current_platform().to_string(),
        config.release_version.clone(),
    );
    let request = CreatePatchInstallEventRequest { event };

    let patch_install_success_fn = config.network_hooks.patch_install_success_fn;
    let url = &patches_events_url(&config.base_url);
    patch_install_success_fn(url, request)
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
    use crate::network::PatchCheckResponse;

    #[test]
    fn check_patch_request_response_deserialization() {
        let data = r###"
    {
        "patch_available": true,
        "patch": {
            "number": 1,
            "download_url": "https://storage.googleapis.com/patch_artifacts/17a28ec1-00cf-452d-bdf9-dbb9acb78600/dlc.vmcode",
            "hash": "#"
        }
    }"###;

        let response: PatchCheckResponse = serde_json::from_str(data).unwrap();

        assert!(response.patch_available == true);
        assert!(response.patch.is_some());

        let patch = response.patch.unwrap();
        assert_eq!(patch.number, 1);
        assert_eq!(patch.download_url, "https://storage.googleapis.com/patch_artifacts/17a28ec1-00cf-452d-bdf9-dbb9acb78600/dlc.vmcode");
        assert_eq!(patch.hash, "#");
    }

    #[test]
    fn create_patch_install_event_request_serializes() {
        let request = super::CreatePatchInstallEventRequest {
            event: super::PatchInstallEvent::new(
                "app_id".to_string(),
                "arch".to_string(),
                "client_id".to_string(),
                1,
                "platform".to_string(),
                "release_version".to_string(),
            ),
        };
        let json_string = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json_string,
            r#"{"event":{"app_id":"app_id","arch":"arch","client_id":"client_id","type":"__patch_install__","patch_number":1,"platform":"platform","release_version":"release_version"}}"#
        )
    }

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
        assert!(debug.contains("patch_install_success_fn"));
    }
}
