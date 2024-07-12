// This file's job is to deal with the update_server and network side
// of the updater library.

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::string::ToString;

use crate::config::UpdateConfig;
use crate::events::PatchEvent;

// https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
#[cfg(test)]
use std::{println as info, println as debug}; // Workaround to use println! for logs.

pub fn patches_check_url(base_url: &str) -> String {
    format!("{base_url}/api/v1/patches/check")
}

fn patches_events_url(base_url: &str) -> String {
    format!("{base_url}/api/v1/patches/events")
}

pub type PatchCheckRequestFn = fn(&str, PatchCheckRequest) -> anyhow::Result<PatchCheckResponse>;
pub type DownloadFileFn = fn(&str) -> anyhow::Result<Vec<u8>>;
pub type ReportEventFn = fn(&str, CreatePatchEventRequest) -> anyhow::Result<()>;

/// A container for network callbacks which can be mocked out for testing.
#[derive(Clone)]
pub struct NetworkHooks {
    /// The function to call to send a patch check request.
    pub patch_check_request_fn: PatchCheckRequestFn,
    /// The function to call to download a file.
    pub download_file_fn: DownloadFileFn,
    /// The function to call to report patch install success.
    pub report_event_fn: ReportEventFn,
}

// We have to implement Debug by hand since fn types don't implement it.
impl core::fmt::Debug for NetworkHooks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkHooks")
            .field("patch_check_request_fn", &"<fn>")
            .field("download_file_fn", &"<fn>")
            .field("report_event_fn", &"<fn>")
            .finish()
    }
}

impl Default for NetworkHooks {
    fn default() -> Self {
        Self {
            patch_check_request_fn: patch_check_request_default,
            download_file_fn: download_file_default,
            report_event_fn: report_event_default,
        }
    }
}

pub fn patch_check_request_default(
    url: &str,
    request: PatchCheckRequest,
) -> anyhow::Result<PatchCheckResponse> {
    info!("Sending patch check request: {:?}", request);
    let client = reqwest::blocking::Client::new();
    let result = client.post(url).json(&request).send();
    let response = handle_network_result(result)?.json()?;
    Ok(response)
}

pub fn download_file_default(url: &str) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::new();
    let result = client.get(url).send();
    let response = handle_network_result(result)?;
    let bytes = response.bytes()?;
    // Patch files are small (e.g. 50kb) so this should be ok to copy into memory.
    Ok(bytes.to_vec())
}

pub fn report_event_default(url: &str, request: CreatePatchEventRequest) -> anyhow::Result<()> {
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
    report_event_fn: ReportEventFn,
) {
    crate::config::with_config_mut(|maybe_config| match maybe_config {
        Some(config) => {
            config.network_hooks = NetworkHooks {
                patch_check_request_fn,
                download_file_fn,
                report_event_fn,
            };
        }
        None => {
            panic!("testing_set_network_hooks called before config was initialized");
        }
    });
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Patch {
    /// The patch number.  Starts at 1 for each new release and increases
    /// monotonically.
    pub number: usize,
    /// The hex-encoded sha256 hash of the final uncompressed patch file.
    /// Legacy: originally "#" before we implemented hash checks (remove).
    pub hash: String,
    /// The URL to download the patch file from.
    pub download_url: String,
    /// The signature of `hash`, if this patch is signed. None otherwise.
    #[serde(default)]
    pub hash_signature: Option<String>,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct PatchCheckResponse {
    pub patch_available: bool,
    #[serde(default)]
    pub patch: Option<Patch>,
}

pub fn send_patch_event(event: PatchEvent, config: &UpdateConfig) -> anyhow::Result<()> {
    let request = CreatePatchEventRequest { event };

    let report_event_fn = config.network_hooks.report_event_fn;
    let url = &patches_events_url(&config.base_url);
    report_event_fn(url, request)
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
    use crate::{network::PatchCheckResponse, time};

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
            patch_number: 1,
            platform: "platform".to_string(),
            release_version: "release_version".to_string(),
            identifier: EventType::PatchInstallSuccess,
            timestamp: 1234,
        };
        let request = super::CreatePatchEventRequest { event };
        let json_string = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json_string,
            r#"{"event":{"app_id":"app_id","arch":"arch","type":"__patch_install__","patch_number":1,"platform":"platform","release_version":"release_version","timestamp":1234}}"#
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
        assert!(debug.contains("report_event_fn"));
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
            patch_number: 2,
            platform: "platform".to_string(),
            release_version: "release_version".to_string(),
            identifier: EventType::PatchInstallSuccess,
            timestamp: time::unix_timestamp(),
        };
        let result = super::report_event_default(
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
        let result = super::report_event_default(
            // Make the request to an incorrectly formatted URL, which will
            // trigger the same error as a lack of internet connection.
            &patches_events_url("asdfasdf"),
            super::CreatePatchEventRequest {
                event: PatchEvent {
                    app_id: "app_id".to_string(),
                    arch: "arch".to_string(),
                    patch_number: 2,
                    platform: "platform".to_string(),
                    release_version: "release_version".to_string(),
                    identifier: EventType::PatchInstallSuccess,
                    timestamp: time::unix_timestamp(),
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
