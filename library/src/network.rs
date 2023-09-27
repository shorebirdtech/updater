// This file's job is to deal with the update_server and network side
// of the updater library.

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::string::ToString;

use crate::cache::UpdaterState;
use crate::config::{current_arch, current_platform, UpdateConfig};
use crate::events::PatchEvent;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as debug}; // Workaround to use println! for logs.

fn patches_check_url(base_url: &str) -> String {
    format!("{base_url}/api/v1/patches/check")
}

fn patches_events_url(base_url: &str) -> String {
    format!("{base_url}/api/v1/patches/events")
}

pub type PatchCheckRequestFn = fn(&str, PatchCheckRequest) -> anyhow::Result<PatchCheckResponse>;
pub type DownloadFileFn = fn(&str) -> anyhow::Result<Vec<u8>>;
pub type PatchInstallSuccessFn = fn(&str, CreatePatchEventRequest) -> anyhow::Result<()>;

/// A container for network callbacks which can be mocked out for testing.
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
    bail!("please set a patch_check_request_fn");
}

#[cfg(test)]
fn download_file_throws(_url: &str) -> anyhow::Result<Vec<u8>> {
    bail!("please set a download_file_fn");
}

#[cfg(test)]
pub fn patch_install_success_throws(
    _url: &str,
    _request: CreatePatchEventRequest,
) -> anyhow::Result<()> {
    bail!("please set a patch_install_success_fn");
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
    let result = client.post(url).json(&request).send();
    let response = handle_network_result(result)?.json()?;
    Ok(response)
}

#[cfg(not(test))]
pub fn download_file_default(url: &str) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::new();
    let result = client.get(url).send();
    let response = handle_network_result(result)?;
    let bytes = response.bytes()?;
    // Patch files are small (e.g. 50kb) so this should be ok to copy into memory.
    Ok(bytes.to_vec())
}

pub fn patch_install_success_default(
    url: &str,
    request: CreatePatchEventRequest,
) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::new();
    let result = client.post(url).json(&request).send();
    handle_network_result(result)?;
    Ok(())
}

/// Handles the result of a network request, returning the response if it was
/// successful, an error if it was not, or a special error if the network
/// request failed due to a lack of internet connection.
fn handle_network_result(
    result: Result<reqwest::blocking::Response, reqwest::Error>,
) -> anyhow::Result<reqwest::blocking::Response> {
    use std::error::Error;

    match result {
        Ok(response) => {
            if response.status().is_success() {
                Ok(response)
            } else {
                bail!("Request failed with status: {}", response.status())
            }
        }
        Err(e) => match e.source() {
            Some(source)
                if source
                    .to_string()
                    .contains("failed to lookup address information") =>
            {
                bail!("Patch check request failed due to network error. Please check your internet connection.");
            }
            _ => bail!(e),
        },
    }
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

/// Any edits to this struct should be made carefully and in accordance
/// with our privacy policy:
/// <https://docs.shorebird.dev/privacy>
/// The request body for the patch check endpoint.
#[derive(Debug, Serialize)]
pub struct PatchCheckRequest {
    /// The Shorebird app_id built into the shorebird.yaml in the app.
    /// app_ids are unique to each app and are used to identify the app
    /// within Shorebird's system (similar to a bundle identifier).  They
    /// are not secret and are safe to share publicly.
    /// <https://docs.shorebird.dev/concepts>
    pub app_id: String,
    /// The Shorebird channel built into the shorebird.yaml in the app.
    /// This is not currently used, but intended for future use to allow
    /// staged rollouts of patches.
    pub channel: String,
    /// The release version from AndroidManifest.xml, Info.plist in the app.
    /// This is used to identify the version of the app that the client is
    /// running.  Patches are keyed to release versions and will only be
    /// offered to clients running the same release version.
    pub release_version: String,
    /// The latest patch number that the client has downloaded.
    /// Not necessarily the one it's running (if some have been marked bad).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_number: Option<usize>,
    /// Platform (e.g. "android", "ios", "windows", "macos", "linux").
    pub platform: String,
    /// Architecture we're running (e.g. "aarch64", "x86", "x86_64").
    pub arch: String,
}

/// The request body for the create patch install event endpoint.
///
/// We may want to consider making this more generic if/when we add more events
/// using something like <https://github.com/dtolnay/typetag>.
#[derive(Debug, Serialize)]
pub struct CreatePatchEventRequest {
    event: PatchEvent,
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
    // Dumping the request should be info! since we direct users to look for it
    // in the logs: https://docs.shorebird.dev/troubleshooting#how-to-fix-it-1
    // Another option would be to make verbosity configurable via a key
    // in shorebird.yaml.
    info!("Sending patch check request: {:?}", request);
    let url = &patches_check_url(&config.base_url);
    let patch_check_request_fn = config.network_hooks.patch_check_request_fn;
    let response = patch_check_request_fn(url, request)?;

    debug!("Patch check response: {:?}", response);
    Ok(response)
}

pub fn send_patch_event(event: PatchEvent, config: &UpdateConfig) -> anyhow::Result<()> {
    let request = CreatePatchEventRequest { event };

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
    let bytes = download_file_hook(url)?;
    // Ensure the download directory exists.
    if let Some(parent) = path.parent() {
        debug!("Creating download directory: {:?}", parent);
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all failed for {}", parent.display()))?;
    }

    debug!("Writing download to: {:?}", path);
    let mut file = File::create(path)?;
    file.write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::network::PatchCheckResponse;

    use super::{patches_events_url, PatchEvent};
    use crate::events::EventType;

    #[test]
    fn check_patch_request_response_deserialization() {
        let data = r#"
    {
        "patch_available": true,
        "patch": {
            "number": 1,
            "download_url": "https://storage.googleapis.com/patch_artifacts/17a28ec1-00cf-452d-bdf9-dbb9acb78600/dlc.vmcode",
            "hash": "1234"
        }
    }"#;

        let response: PatchCheckResponse = serde_json::from_str(data).unwrap();

        assert!(response.patch_available);
        assert!(response.patch.is_some());

        let patch = response.patch.unwrap();
        assert_eq!(patch.number, 1);
        assert_eq!(patch.download_url, "https://storage.googleapis.com/patch_artifacts/17a28ec1-00cf-452d-bdf9-dbb9acb78600/dlc.vmcode");
        assert_eq!(patch.hash, "1234");
    }

    #[test]
    fn create_patch_install_event_request_serializes() {
        let event = PatchEvent {
            app_id: "app_id".to_string(),
            arch: "arch".to_string(),
            client_id: "client_id".to_string(),
            patch_number: 1,
            platform: "platform".to_string(),
            release_version: "release_version".to_string(),
            identifier: EventType::PatchInstallSuccess,
        };
        let request = super::CreatePatchEventRequest { event };
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

    #[test]
    fn handle_network_result_ok() {
        let http_response = http::response::Builder::new()
            .status(200)
            .body("".to_string())
            .unwrap();
        let response = reqwest::blocking::Response::from(http_response);

        let result = super::handle_network_result(Ok(response));

        assert!(result.is_ok());
    }

    #[test]
    fn handle_network_result_http_status_not_ok() {
        let http_response = http::response::Builder::new()
            .status(500)
            .body("".to_string())
            .unwrap();
        let response = reqwest::blocking::Response::from(http_response);

        let result = super::handle_network_result(Ok(response));

        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.to_string(),
            "Request failed with status: 500 Internal Server Error"
        );
    }

    #[test]
    fn handle_network_result_no_internet() {
        let event = PatchEvent {
            app_id: "app_id".to_string(),
            arch: "arch".to_string(),
            client_id: "client_id".to_string(),
            patch_number: 2,
            platform: "platform".to_string(),
            release_version: "release_version".to_string(),
            identifier: EventType::PatchInstallSuccess,
        };
        let result = super::patch_install_success_default(
            // Make the request to a non-existent URL, which will trigger the
            // same error as a lack of internet connection.
            &patches_events_url("http://asdfasdfasdfasdfasdf.asdfasdf"),
            super::CreatePatchEventRequest { event },
        );

        assert!(result.is_err());
        let error = result.err().unwrap();
        assert_eq!(error.to_string(), "Patch check request failed due to network error. Please check your internet connection.")
    }

    #[test]
    fn handle_network_result_unknown_error() {
        let result = super::patch_install_success_default(
            // Make the request to an incorrectly formatted URL, which will
            // trigger the same error as a lack of internet connection.
            &patches_events_url("asdfasdf"),
            super::CreatePatchEventRequest {
                event: PatchEvent {
                    app_id: "app_id".to_string(),
                    arch: "arch".to_string(),
                    client_id: "client_id".to_string(),
                    patch_number: 2,
                    platform: "platform".to_string(),
                    release_version: "release_version".to_string(),
                    identifier: EventType::PatchInstallSuccess,
                },
            },
        );

        assert!(result.is_err());
        let error = result.err().unwrap();
        assert_eq!(
            error.to_string(),
            "builder error: relative URL without a base"
        )
    }
}
