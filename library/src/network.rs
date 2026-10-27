// This file's job is to deal with the update_server and network side
// of the updater library.

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
    shorebird_info!("Sending patch check request: {}", request.log_line());
    let result = ureq::post(url)
        .config()
        .http_status_as_error(false)
        .build()
        .send_json(&request);
    let response = handle_network_result(result)?;
    // A 2xx that isn't our JSON came from something between the device and
    // us (a captive portal, a filtering proxy), not from our server.
    let parsed = response.into_body().read_json().map_err(|e| match e {
        e @ ureq::Error::Json(_) => anyhow::Error::new(e).context(NetworkError::UnexpectedResponse),
        e => from_ureq_error(e),
    })?;
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
    let mut request = ureq::get(url).config().http_status_as_error(false).build();
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

/// How long an event post may take. The flush that sends it holds a lock
/// other flushes wait on, so one that never gets an answer must give up.
const REPORT_EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub fn report_event_default(url: &str, request: CreatePatchEventRequest) -> anyhow::Result<()> {
    let result = ureq::post(url)
        .config()
        .timeout_global(Some(REPORT_EVENT_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .send_json(&request);
    handle_network_result(result)?;
    Ok(())
}

/// A typed network-layer failure.
///
/// Preserving the failure as a concrete type (rather than a `bail!`-ed string)
/// lets the failure classifier in `updater.rs` distinguish "the server answered
/// with an error status" from "we never reached the server". This is internal
/// enabling work for the categorized-failure surface planned in the
/// `shorebird_code_push` v3 PRD (Change #7: sealed `Failed{category, code,
/// retryable}`). It is intentionally *not* exposed across the C ABI — the FFI
/// still flattens every failure to `SHOREBIRD_UPDATE_ERROR` today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NetworkError {
    /// The server was reached but responded with a non-success HTTP status.
    /// `error` is the server's own explanation, when the body was ours.
    Server {
        status: u16,
        error: Option<ServerError>,
    },
    /// The request never reached the server: offline, DNS failure, connection
    /// refused, or a transport-level IO error.
    Transport,
    /// A success status whose body wasn't our JSON, so something other than
    /// our server answered (a captive portal or a proxy).
    UnexpectedResponse,
}

/// The `{code, message}` body our server sends with an error status, such as
/// `patches_check_limit_exceeded` when an app is over its install limit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct ServerError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // These messages are not shown to end users — they surface only in logs
        // and the server-bound failure diagnostic event. They must stay generic
        // across all network operations (patch check, download, event report).
        match self {
            NetworkError::Server {
                status,
                error: Some(error),
            } => write!(f, "{} ({}, status {status})", error.message, error.code),
            // Nothing of ours answers 429; it comes from infrastructure in
            // front of the server shedding load.
            NetworkError::Server {
                status: 429,
                error: None,
            } => write!(f, "The server is busy (status 429); try again later."),
            NetworkError::Server {
                status,
                error: None,
            } => write!(f, "Request failed with status: {status}"),
            NetworkError::Transport => {
                write!(
                    f,
                    "Network request failed; the server could not be reached."
                )
            }
            NetworkError::UnexpectedResponse => write!(
                f,
                "The response was not from the Shorebird server; a captive portal or proxy may be answering in its place."
            ),
        }
    }
}

impl std::error::Error for NetworkError {}

/// Error bodies are a short JSON object; read no more than this of one.
const MAX_ERROR_BODY_BYTES: u64 = 16 * 1024;

/// Maps a raw `ureq` result into our typed [`NetworkError`] (or preserves the
/// underlying `ureq` error for the uncommon cases), returning the response on
/// success. Requests must set `http_status_as_error(false)`, so an error
/// status arrives here with its body and the server's explanation is kept.
fn handle_network_result(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
    let response = result.map_err(from_ureq_error)?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    // A body that isn't ours (a load balancer's HTML page) is no explanation.
    let error = response
        .into_body()
        .with_config()
        .limit(MAX_ERROR_BODY_BYTES)
        .read_json::<ServerError>()
        .ok();
    Err(NetworkError::Server {
        status: status.as_u16(),
        error,
    }
    .into())
}

fn from_ureq_error(error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::StatusCode(status) => NetworkError::Server {
            status,
            error: None,
        }
        .into(),
        // Keep ureq's error as the cause, so logs and the failure event say
        // why the server couldn't be reached (an untrusted certificate, a DNS
        // failure, a refused connection) instead of only that it couldn't.
        e @ (ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Io(_)) => {
            anyhow::Error::new(e).context(NetworkError::Transport)
        }
        e => anyhow::Error::new(e),
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
#[derive(Serialize)]
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
    /// The anonymous install identifier. A random UUID generated by Shorebird,
    /// per install rather than per device, and _not_ the device's UUID or any
    /// other identifier that has meaning outside of Shorebird.
    pub client_id: String,
    /// The number of the patch currently running on the device, if any.
    ///
    /// Supersedes the legacy `patch_number` field. `patch_number` is
    /// retained server-side for compatibility with older clients that still
    /// rely on its short-circuit response path; newer updaters populate
    /// this field instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_patch_number: Option<usize>,
}

/// Hand-written so `client_id` cannot reach a log through a `{:?}` render of
/// this struct. The field still serializes, since it is part of the patch
/// check payload the server counts installs from.
impl std::fmt::Debug for PatchCheckRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Destructured without `..` so a field added to this struct fails to
        // compile here rather than silently vanishing from the render.
        let Self {
            app_id,
            channel,
            release_version,
            platform,
            arch,
            client_id: _,
            current_patch_number,
        } = self;
        f.debug_struct("PatchCheckRequest")
            .field("app_id", app_id)
            .field("channel", channel)
            .field("release_version", release_version)
            .field("platform", platform)
            .field("arch", arch)
            .field("client_id", &"<redacted>")
            .field("current_patch_number", current_patch_number)
            .finish()
    }
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

    /// The fields of this request that are safe to log, in the order
    /// troubleshooting asks users to read them off the line.
    ///
    /// Split out from the log call because `shorebird_info!` expands to
    /// `println!` under `cfg(test)`, so a test cannot assert what the macro
    /// emitted. Returning the line lets a test check that `client_id` is
    /// absent from it.
    fn log_line(&self) -> String {
        // Destructured without `..` so a field added to this struct fails to
        // compile here rather than silently vanishing from the line.
        let Self {
            app_id,
            channel,
            release_version,
            platform,
            arch,
            client_id: _,
            current_patch_number,
        } = self;
        format!(
            "app_id: {app_id}, channel: {channel}, release_version: {release_version}, platform: {platform}, arch: {arch}, current_patch_number: {current_patch_number:?}"
        )
    }
}

/// The request body for the create patch install event endpoint.
///
/// We may want to consider making this more generic if/when we add more events
/// using something like <https://github.com/dtolnay/typetag>.
#[derive(Debug, Serialize)]
pub struct CreatePatchEventRequest {
    pub(crate) event: PatchEvent,
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
    fn patch_check_request_debug_masks_client_id() {
        let request = super::PatchCheckRequest {
            app_id: "app_id".to_string(),
            channel: "stable".to_string(),
            release_version: "1.0.0+1".to_string(),
            platform: "android".to_string(),
            arch: "aarch64".to_string(),
            client_id: "11111111-2222-3333-4444-555555555555".to_string(),
            current_patch_number: Some(3),
        };
        let debug = format!("{:?}", request);
        assert!(!debug.contains("11111111-2222-3333-4444-555555555555"));
        assert!(debug.contains("client_id: \"<redacted>\""));
        assert!(debug.contains("channel: \"stable\""));
    }

    #[test]
    fn patch_check_request_log_line_omits_client_id() {
        let request = super::PatchCheckRequest {
            app_id: "app_id".to_string(),
            channel: "stable".to_string(),
            release_version: "1.0.0+1".to_string(),
            platform: "android".to_string(),
            arch: "aarch64".to_string(),
            client_id: "11111111-2222-3333-4444-555555555555".to_string(),
            current_patch_number: Some(3),
        };
        // Pinned whole rather than by substring: the docs examples of this
        // line are matched to it character for character.
        assert_eq!(
            request.log_line(),
            "app_id: app_id, channel: stable, release_version: 1.0.0+1, platform: android, arch: aarch64, current_patch_number: Some(3)"
        );
        assert!(!request.log_line().contains("client_id"));
    }

    #[test]
    fn patch_check_request_serializes_client_id() {
        let request = super::PatchCheckRequest {
            app_id: "app_id".to_string(),
            channel: "stable".to_string(),
            release_version: "1.0.0+1".to_string(),
            platform: "android".to_string(),
            arch: "aarch64".to_string(),
            client_id: "11111111-2222-3333-4444-555555555555".to_string(),
            current_patch_number: Some(3),
        };
        let json_string = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json_string,
            r#"{"app_id":"app_id","channel":"stable","release_version":"1.0.0+1","platform":"android","arch":"aarch64","client_id":"11111111-2222-3333-4444-555555555555","current_patch_number":3}"#
        );
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
            failure_category: None,
            event_id: None,
        };
        let request = super::CreatePatchEventRequest { event };
        let json_string = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json_string,
            r#"{"event":{"app_id":"app_id","arch":"arch","client_id":"client_id","type":"__patch_install__","patch_number":1,"platform":"platform","release_version":"release_version","timestamp":1234,"message":null}}"#
        )
    }

    #[test]
    fn create_patch_event_request_serializes_with_failure_category() {
        // Locks the wire key (`failure_category`) the server reads. When None,
        // the field is omitted entirely (see the other serialization tests).
        let event = PatchEvent {
            app_id: "app_id".to_string(),
            client_id: "client_id".to_string(),
            arch: "arch".to_string(),
            patch_number: 1,
            platform: "platform".to_string(),
            release_version: "release_version".to_string(),
            identifier: EventType::PatchUpdateFailure,
            timestamp: 1234,
            message: Some("update_failure: patch 1 failed (...)".to_string()),
            failure_category: Some("network".to_string()),
            event_id: None,
        };
        let json_string = serde_json::to_string(&event).unwrap();
        assert!(
            json_string.contains(r#""failure_category":"network""#),
            "expected failure_category in: {json_string}"
        );
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
            failure_category: None,
            event_id: None,
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
            "Network request failed; the server could not be reached."
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

    fn response(status: u16, mime_type: &str, body: &str) -> ureq::http::Response<ureq::Body> {
        let body = ureq::Body::builder()
            .mime_type(mime_type)
            .data(body.to_string());
        ureq::http::Response::builder()
            .status(status)
            .body(body)
            .unwrap()
    }

    #[test]
    fn handle_network_result_keeps_the_servers_error() {
        // What the patch check answers when an app is over its install limit.
        let body = r#"{"code":"patches_check_limit_exceeded","message":"The number of installs for this app has exceeded the limit.","details":"app_id: abc"}"#;
        let err =
            super::handle_network_result(Ok(response(403, "application/json", body))).unwrap_err();

        assert_eq!(
            err.downcast_ref::<super::NetworkError>(),
            Some(&super::NetworkError::Server {
                status: 403,
                error: Some(super::ServerError {
                    code: "patches_check_limit_exceeded".to_string(),
                    message: "The number of installs for this app has exceeded the limit."
                        .to_string(),
                }),
            })
        );
        assert_eq!(
            err.to_string(),
            "The number of installs for this app has exceeded the limit. \
             (patches_check_limit_exceeded, status 403)"
        );
    }

    #[test]
    fn handle_network_result_ignores_an_error_body_that_isnt_ours() {
        let err = super::handle_network_result(Ok(response(
            502,
            "text/html",
            "<html>Bad Gateway</html>",
        )))
        .unwrap_err();

        assert_eq!(err.to_string(), "Request failed with status: 502");
    }

    #[test]
    fn handle_network_result_429_says_the_server_is_busy() {
        let err =
            super::handle_network_result(Ok(response(429, "text/plain", "Too Many Requests")))
                .unwrap_err();

        assert_eq!(
            err.to_string(),
            "The server is busy (status 429); try again later."
        );
    }

    /// Answers one HTTP request on localhost with `status` and `body`, and
    /// returns the base URL to send it to.
    fn serve_once(status: &str, content_type: &str, body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read the request before answering, so the client's send finishes.
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stream.read(&mut buf) {
                request.extend_from_slice(&buf[..n]);
                if n == 0 || request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = stream.write_all(response.as_bytes());
        });
        url
    }

    fn check_request() -> super::PatchCheckRequest {
        super::PatchCheckRequest {
            app_id: "app_id".to_string(),
            channel: "stable".to_string(),
            release_version: "1.0.0+1".to_string(),
            platform: "android".to_string(),
            arch: "aarch64".to_string(),
            client_id: "client_id".to_string(),
            current_patch_number: None,
        }
    }

    #[test]
    fn patch_check_over_the_limit_says_so() {
        let base_url = serve_once(
            "403 Forbidden",
            "application/json",
            r#"{"code":"patches_check_limit_exceeded","message":"The number of installs for this app has exceeded the limit."}"#,
        );

        let err = super::patch_check_request_default(
            &super::patches_check_url(&base_url),
            check_request(),
        )
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "The number of installs for this app has exceeded the limit. \
             (patches_check_limit_exceeded, status 403)"
        );
    }

    #[test]
    fn patch_check_answered_by_a_captive_portal_is_unexpected_response() {
        let base_url = serve_once("200 OK", "text/html", "<html>Sign in to Wi-Fi</html>");

        let err = super::patch_check_request_default(
            &super::patches_check_url(&base_url),
            check_request(),
        )
        .unwrap_err();

        assert_eq!(
            err.downcast_ref::<super::NetworkError>(),
            Some(&super::NetworkError::UnexpectedResponse)
        );
    }

    #[test]
    fn handle_network_result_passes_partial_content() {
        assert!(super::handle_network_result(Ok(response(206, "text/plain", ""))).is_ok());
    }

    #[test]
    fn handle_network_result_keeps_transport_cause() {
        // What ureq reports when a TLS-inspecting proxy presents a root that
        // isn't in the updater's trust store.
        let io = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid peer certificate: UnknownIssuer",
        );
        let err = super::handle_network_result(Err(ureq::Error::Io(io))).unwrap_err();

        assert_eq!(
            err.downcast_ref::<super::NetworkError>(),
            Some(&super::NetworkError::Transport)
        );
        assert_eq!(
            format!("{err:#}"),
            "Network request failed; the server could not be reached.: \
             io: invalid peer certificate: UnknownIssuer"
        );
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
            failure_category: None,
            event_id: None,
        };
        let result = super::report_event_default(
            // Make the request to a non-existent URL, which will trigger the
            // same error as a lack of internet connection.
            &patches_events_url("http://asdfasdfasdfasdfasdf.asdfasdf"),
            super::CreatePatchEventRequest { event },
        );

        assert!(result.is_err());
        let error = result.err().unwrap();
        assert_eq!(
            error.to_string(),
            "Network request failed; the server could not be reached."
        )
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
                    failure_category: None,
                    event_id: None,
                },
            },
        );

        assert!(result.is_err());
    }
}
