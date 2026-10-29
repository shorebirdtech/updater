// This file's job is to deal with the update_server and network side
// of the updater library.

use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
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

    copy_body_to_file(&mut response.into_body().as_reader(), &mut file, dest)?;

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
    Server { status: u16 },
    /// The request never reached the server (offline, DNS failure, connection
    /// refused), or the connection failed mid-response (the peer disconnected,
    /// a timeout).
    Transport,
}

impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // These messages are not shown to end users — they surface only in logs
        // and the server-bound failure diagnostic event. They must stay generic
        // across all network operations (patch check, download, event report).
        match self {
            NetworkError::Server { status } => write!(f, "Request failed with status: {status}"),
            NetworkError::Transport => {
                write!(
                    f,
                    "Network request failed; the server could not be reached."
                )
            }
        }
    }
}

impl std::error::Error for NetworkError {}

/// Maps a raw `ureq` result into our typed [`NetworkError`] (or preserves the
/// underlying `ureq` error for the uncommon cases), returning the response on
/// success.
fn handle_network_result(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::StatusCode(code)) => Err(NetworkError::Server { status: code }.into()),
        // Keep ureq's error as the cause, so logs and the failure event say
        // why the server couldn't be reached (an untrusted certificate, a DNS
        // failure, a refused connection) instead of only that it couldn't.
        Err(
            e @ (ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Io(_)),
        ) => Err(anyhow::Error::new(e).context(NetworkError::Transport)),
        Err(e) => bail!(e),
    }
}

/// Streams a response body into `file`, keeping read and write failures
/// apart: a body that stops mid-download (the peer disconnected, the
/// connection timed out) is a network failure, and only a failed write is a
/// file failure. `std::io::copy` returns both as one `io::Error`, which
/// reported every dropped connection as "Failed to write to file".
fn copy_body_to_file(
    body: &mut impl Read,
    file: &mut impl Write,
    dest: &Path,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(anyhow::Error::new(e).context(NetworkError::Transport)),
        };
        file.write_all(&buf[..n])
            .with_file_context(FileOperation::WriteFile, dest)?;
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
/// <https://shorebird.dev/privacy/>
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
    use std::path::Path;

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

    /// A body that yields `data`, then fails the way ureq does when the
    /// server drops the connection mid-download.
    struct DroppedBody<'a> {
        data: &'a [u8],
    }

    impl std::io::Read for DroppedBody<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.data.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Peer disconnected",
                ));
            }
            let n = self.data.len().min(buf.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    /// A file whose writes fail because the disk is full.
    struct FullDisk;

    impl std::io::Write for FullDisk {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn copy_body_to_file_copies_whole_body() {
        let mut out = Vec::new();
        super::copy_body_to_file(&mut &b"patch bytes"[..], &mut out, Path::new("dest")).unwrap();
        assert_eq!(out, b"patch bytes");
    }

    #[test]
    fn copy_body_to_file_dropped_connection_is_network() {
        let mut out = Vec::new();
        let err = super::copy_body_to_file(
            &mut DroppedBody { data: b"partial" },
            &mut out,
            Path::new("dest"),
        )
        .unwrap_err();

        // The bytes before the drop are kept, so the next attempt resumes.
        assert_eq!(out, b"partial");
        assert_eq!(
            crate::updater::classify_failure(&err),
            crate::updater::FailureCategory::Network
        );
        assert_eq!(
            format!("{err:#}"),
            "Network request failed; the server could not be reached.: Peer disconnected"
        );
    }

    #[test]
    fn copy_body_to_file_write_failure_is_file_error() {
        let err =
            super::copy_body_to_file(&mut &b"patch bytes"[..], &mut FullDisk, Path::new("dest"))
                .unwrap_err();

        assert_eq!(
            crate::updater::classify_failure(&err),
            crate::updater::FailureCategory::OutOfSpace
        );
        assert!(err
            .to_string()
            .starts_with("Failed to write to file 'dest'"));
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
