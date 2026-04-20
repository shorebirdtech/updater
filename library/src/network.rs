// This file's job is to deal with the update_server and network side
// of the updater library.

use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::string::ToString;

use crate::config::{current_arch, current_platform, UpdateConfig};
use crate::events::PatchEvent;
use crate::file_errors::{FileOperation, IoResultExt};

pub fn patches_check_url(base_url: &str) -> String {
    format!("{base_url}/api/v1/patches/check")
}

fn patches_events_url(base_url: &str) -> String {
    format!("{base_url}/api/v1/patches/events")
}

pub type PatchCheckRequestFn = fn(&str, PatchCheckRequest) -> anyhow::Result<PatchCheckResponse>;
pub type DownloadToPathFn =
    fn(url: &str, dest: &Path, resume_from: u64) -> anyhow::Result<DownloadResult>;
pub type ReportEventFn = fn(&str, CreatePatchEventRequest) -> anyhow::Result<()>;

/// Result of a download operation.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    /// Total bytes written to the file (including any previously downloaded bytes on resume).
    pub total_bytes: u64,
    /// The Content-Length from the server response, if present.
    pub content_length: Option<u64>,
}

/// A container for network callbacks which can be mocked out for testing.
#[derive(Clone)]
pub struct NetworkHooks {
    /// The function to call to send a patch check request.
    pub patch_check_request_fn: PatchCheckRequestFn,
    /// The function to call to download a file.
    pub download_to_path_fn: DownloadToPathFn,
    /// The function to call to report patch install success.
    pub report_event_fn: ReportEventFn,
}

// We have to implement Debug by hand since fn types don't implement it.
impl core::fmt::Debug for NetworkHooks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkHooks")
            .field("patch_check_request_fn", &"<fn>")
            .field("download_to_path_fn", &"<fn>")
            .field("report_event_fn", &"<fn>")
            .finish()
    }
}

impl Default for NetworkHooks {
    fn default() -> Self {
        Self {
            patch_check_request_fn: patch_check_request_default,
            download_to_path_fn: download_to_path_default,
            report_event_fn: report_event_default,
        }
    }
}

pub fn patch_check_request_default(
    url: &str,
    request: PatchCheckRequest,
) -> anyhow::Result<PatchCheckResponse> {
    shorebird_info!("Sending patch check request: {:?}", request);
    let result = ureq::post(url).send_json(&request);
    let response = handle_network_result(result)?;
    let parsed = response.into_body().read_json()?;
    shorebird_debug!("Patch check response: {:?}", parsed);
    Ok(parsed)
}

/// Default download implementation that streams to a file with Range header
/// support for resuming partial downloads.
pub fn download_to_path_default(
    url: &str,
    dest: &Path,
    resume_from: u64,
) -> anyhow::Result<DownloadResult> {
    let mut request = ureq::get(url);
    if resume_from > 0 {
        request = request.header("Range", &format!("bytes={resume_from}-"));
    }
    let result = request.call();
    let response = handle_network_result(result)?;
    let status = response.status();

    // Determine total file size from headers.
    let content_length = if status == 206 {
        parse_content_range_total(response.headers())
    } else {
        // 200: Content-Length is the full file size.
        response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    };

    // Only resume (append) when the server actually returned 206.
    // If the server ignored our Range header and returned 200, start fresh.
    let mut file = if status == 206 && resume_from > 0 {
        let mut f = OpenOptions::new()
            .write(true)
            .open(dest)
            .with_file_context(FileOperation::WriteFile, dest)?;
        f.seek(SeekFrom::Start(resume_from))
            .with_file_context(FileOperation::WriteFile, dest)?;
        f
    } else {
        // Fresh download (200 OK or server ignored Range): create/truncate.
        File::create(dest).with_file_context(FileOperation::CreateFile, dest)?
    };

    std::io::copy(&mut response.into_body().as_reader(), &mut file)
        .with_file_context(FileOperation::WriteFile, dest)?;

    let total_bytes = std::fs::metadata(dest)
        .with_file_context(FileOperation::ReadFile, dest)?
        .len();

    Ok(DownloadResult {
        total_bytes,
        content_length,
    })
}

pub fn report_event_default(url: &str, request: CreatePatchEventRequest) -> anyhow::Result<()> {
    let result = ureq::post(url).send_json(&request);
    handle_network_result(result)?;
    Ok(())
}

/// Handles the result of a network request, returning the response if it was
/// successful, an error if it was not, or a special error if the network
/// request failed due to a lack of internet connection.
fn handle_network_result(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::StatusCode(code)) => {
            bail!("Request failed with status: {}", code)
        }
        Err(ureq::Error::HostNotFound)
        | Err(ureq::Error::ConnectionFailed)
        | Err(ureq::Error::Io(_)) => {
            // TODO: This message says "Patch check request" even when the
            // failure is a download or event report.
            bail!("Patch check request failed due to network error. Please check your internet connection.");
        }
        Err(e) => bail!(e),
    }
}

/// Parses the total file size from a Content-Range header.
/// Expected format: `bytes start-end/total` (e.g. `bytes 100-199/1000`).
/// Returns `None` if the header is missing, malformed, or the total is `*`.
fn parse_content_range_total(headers: &ureq::http::HeaderMap) -> Option<u64> {
    headers
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|v| v.parse::<u64>().ok())
}

#[cfg(test)]
/// Panicking placeholder for tests that should never reach the download step.
pub const UNEXPECTED_DOWNLOAD: DownloadToPathFn = |_, _, _| panic!("unexpected download call");

#[cfg(test)]
/// Panicking placeholder for tests that should never reach the report step.
#[allow(dead_code)]
pub const UNEXPECTED_REPORT: ReportEventFn = |_, _| panic!("unexpected report event call");

#[cfg(test)]
/// Unit tests can call this to mock out the network calls. Use
/// `UNEXPECTED_DOWNLOAD` or `UNEXPECTED_REPORT` for hooks that should
/// not be called in a given test.
pub fn testing_set_network_hooks(
    patch_check_request_fn: PatchCheckRequestFn,
    download_to_path_fn: DownloadToPathFn,
    report_event_fn: ReportEventFn,
) {
    crate::config::with_config_mut(|maybe_config| match maybe_config {
        Some(config) => {
            config.network_hooks = NetworkHooks {
                patch_check_request_fn,
                download_to_path_fn,
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
    /// Platform (e.g. "android", "ios", "windows", "macos", "linux").
    pub platform: String,
    /// Architecture we're running (e.g. "aarch64", "x86", "x86_64").
    pub arch: String,
    /// The unique ID of this device. This is a random UUID generated by Shorebird and _not_ the
    /// device's UUID or any other identifier that has meaning outside of Shorebird.
    pub client_id: String,
    /// The number of the patch currently running on the device, if any.
    ///
    /// Reported for analytics (e.g. MAU breakdowns by patch). This is
    /// intentionally separate from the legacy `patch_number` field, which
    /// older updaters sent and which still triggers a server-side
    /// short-circuit response; sending that field from new updaters would
    /// suppress information (like `rolled_back_patch_numbers`) that we need.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_patch_number: Option<usize>,
}

impl PatchCheckRequest {
    pub fn new(
        config: &UpdateConfig,
        client_id: &str,
        current_patch_number: Option<usize>,
    ) -> PatchCheckRequest {
        PatchCheckRequest {
            app_id: config.app_id.clone(),
            channel: config.channel.clone(),
            release_version: config.release_version.clone(),
            platform: current_platform().to_string(),
            arch: current_arch().to_string(),
            client_id: client_id.to_string(),
            current_patch_number,
        }
    }
}

/// The request body for the create patch install event endpoint.
///
/// We may want to consider making this more generic if/when we add more events
/// using something like <https://github.com/dtolnay/typetag>.
#[derive(Debug, Serialize)]
pub struct CreatePatchEventRequest {
    event: PatchEvent,
}

/// A response from the server telling us the latest state of patches for this release.
#[derive(Debug, Deserialize, Serialize)]
pub struct PatchCheckResponse {
    pub patch_available: bool,
    #[serde(default)]
    pub patch: Option<Patch>,

    /// A list of patch numbers that have been rolled back by app developers. These should be
    /// uninstalled from the device and not booted from.
    #[serde(default)]
    pub rolled_back_patch_numbers: Option<Vec<usize>>,
}

/// Reports a patch event (e.g., install success/failure) to the server.
pub fn send_patch_event(event: PatchEvent, config: &UpdateConfig) -> anyhow::Result<()> {
    let request = CreatePatchEventRequest { event };

    let report_event_fn = config.network_hooks.report_event_fn;
    let url = &patches_events_url(&config.base_url);
    report_event_fn(url, request)
}

/// Downloads the file at `url` to `path`, optionally resuming from byte offset
/// `resume_from`. Ensures the parent directory exists before downloading.
pub fn download_to_path(
    network_hooks: &NetworkHooks,
    url: &str,
    path: &Path,
    resume_from: u64,
) -> anyhow::Result<DownloadResult> {
    shorebird_info!("Downloading patch from: {}", url);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_file_context(FileOperation::CreateDir, parent)?;
    }
    let download_hook = network_hooks.download_to_path_fn;
    let result = download_hook(url, path, resume_from)?;
    shorebird_info!(
        "Downloaded patch to: {:?} ({} bytes)",
        path,
        result.total_bytes
    );
    Ok(result)
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
            client_id: "client_id".to_string(),
            arch: "arch".to_string(),
            patch_number: 1,
            platform: "platform".to_string(),
            release_version: "release_version".to_string(),
            identifier: EventType::PatchInstallSuccess,
            timestamp: 1234,
            message: None,
        };
        let request = super::CreatePatchEventRequest { event };
        let json_string = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json_string,
            r#"{"event":{"app_id":"app_id","arch":"arch","client_id":"client_id","type":"__patch_install__","patch_number":1,"platform":"platform","release_version":"release_version","timestamp":1234,"message":null}}"#
        )
    }

    #[test]
    fn create_patch_install_event_request_serializes_with_message() {
        let event = PatchEvent {
            app_id: "app_id".to_string(),
            client_id: "client_id".to_string(),
            arch: "arch".to_string(),
            patch_number: 1,
            platform: "platform".to_string(),
            release_version: "release_version".to_string(),
            identifier: EventType::PatchInstallSuccess,
            timestamp: 1234,
            message: Some("hello".to_string()),
        };
        let request = super::CreatePatchEventRequest { event };
        let json_string = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json_string,
            r#"{"event":{"app_id":"app_id","arch":"arch","client_id":"client_id","type":"__patch_install__","patch_number":1,"platform":"platform","release_version":"release_version","timestamp":1234,"message":"hello"}}"#
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
                platform: "".to_string(),
                arch: "".to_string(),
                client_id: "".to_string(),
                current_patch_number: None,
            },
        );
        assert!(result.is_err());
        let result = (network_hooks.download_to_path_fn)("", std::path::Path::new("/tmp/test"), 0);
        assert!(result.is_err());
    }

    #[test]
    fn network_hooks_debug() {
        let network_hooks = super::NetworkHooks::default();
        let debug = format!("{:?}", network_hooks);
        assert!(debug.contains("patch_check_request_fn"));
        assert!(debug.contains("download_to_path_fn"));
        assert!(debug.contains("report_event_fn"));
    }

    #[test]
    fn parse_content_range_total_valid() {
        let mut headers = ureq::http::HeaderMap::new();
        headers.insert("content-range", "bytes 100-199/1000".parse().unwrap());
        assert_eq!(super::parse_content_range_total(&headers), Some(1000));
    }

    #[test]
    fn parse_content_range_total_missing() {
        let headers = ureq::http::HeaderMap::new();
        assert_eq!(super::parse_content_range_total(&headers), None);
    }

    #[test]
    fn parse_content_range_total_unknown_size() {
        let mut headers = ureq::http::HeaderMap::new();
        headers.insert("content-range", "bytes 100-199/*".parse().unwrap());
        assert_eq!(super::parse_content_range_total(&headers), None);
    }

    #[test]
    fn download_to_path_no_internet() {
        let dest = std::path::Path::new("/tmp/updater_test_no_internet");
        let result = super::download_to_path_default(
            "http://asdfasdfasdfasdfasdf.asdfasdf/patch/1",
            dest,
            0,
        );
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Patch check request failed due to network error. Please check your internet connection."
        );
    }

    #[test]
    fn handle_network_result_ok() {
        let body = ureq::Body::builder().mime_type("text/plain").data("");
        let response = ureq::http::Response::builder()
            .status(200)
            .body(body)
            .unwrap();

        let result = super::handle_network_result(Ok(response));

        assert!(result.is_ok());
    }

    #[test]
    fn handle_network_result_http_status_not_ok() {
        let result = super::handle_network_result(Err(ureq::Error::StatusCode(500)));

        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(err.to_string(), "Request failed with status: 500");
    }

    #[test]
    fn handle_network_result_no_internet() {
        let event = PatchEvent {
            app_id: "app_id".to_string(),
            client_id: "client_id".to_string(),
            arch: "arch".to_string(),
            patch_number: 2,
            platform: "platform".to_string(),
            release_version: "release_version".to_string(),
            identifier: EventType::PatchInstallSuccess,
            timestamp: time::unix_timestamp(),
            message: None,
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
            // trigger an error.
            &patches_events_url("does_not_exist"),
            super::CreatePatchEventRequest {
                event: PatchEvent {
                    app_id: "app_id".to_string(),
                    client_id: "client_id".to_string(),
                    arch: "arch".to_string(),
                    patch_number: 2,
                    platform: "platform".to_string(),
                    release_version: "release_version".to_string(),
                    identifier: EventType::PatchInstallSuccess,
                    timestamp: time::unix_timestamp(),
                    message: None,
                },
            },
        );

        assert!(result.is_err());
    }
}
