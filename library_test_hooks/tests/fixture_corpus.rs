//! Replays the language-neutral fixture corpus in `fixtures/` against the
//! updater's C API. The format is documented in `fixtures/README.md`; any
//! other implementation of the updater (the C++ port) is expected to pass the
//! same files through its own runner.
//!
//! Everything here goes through the production C symbols plus
//! `testing_reset_config` (a process restart, as far as updater state is
//! concerned). The updater talks to a scripted HTTP server on 127.0.0.1.
//!
//! Set `FIXTURE_FILTER=<substring>` to run only matching cases.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CStr, CString};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use updater::c_api::{dart, engine};

const DEFAULT_RELEASE_VERSION: &str = "1.0.0+1";
const DEFAULT_YAML: &str = "app_id: fixture-app-id\nbase_url: {{server}}\n";
/// How long a step may take to deliver the requests it is expected to make.
/// Some requests (event reports) are sent from a background thread after the
/// C call returns.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to listen for stray requests once a case has finished.
const QUIET_PERIOD: Duration = Duration::from_millis(200);

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

fn host_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else {
        "x86_64"
    }
}

fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.iter().map(|p| p.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Compares `actual` against `expected`, where strings in `expected` of the
/// form `<...>` are matchers (see fixtures/README.md). Objects must have
/// exactly the same keys; arrays the same length.
fn match_json(expected: &Value, actual: &Value, at: &str) -> Result<(), String> {
    if let Value::String(e) = expected {
        let looks_like_matcher = ["<prefix:", "<contains:"].iter().any(|m| e.starts_with(m));
        if looks_like_matcher && !e.ends_with('>') {
            return Err(format!("{at}: matcher {e:?} is missing its closing '>'"));
        }
        if e.starts_with('<') && e.ends_with('>') {
            let inner = &e[1..e.len() - 1];
            let ok = match inner {
                "any" => true,
                "number" => actual.is_u64() || actual.is_i64(),
                "string" => actual.is_string(),
                "uuid" => actual.as_str().is_some_and(is_uuid),
                "platform" => actual.as_str() == Some(host_platform()),
                "arch" => actual.as_str() == Some(host_arch()),
                _ => match inner.split_once(':') {
                    Some(("prefix", p)) => actual.as_str().is_some_and(|s| s.starts_with(p)),
                    Some(("contains", p)) => actual.as_str().is_some_and(|s| s.contains(p)),
                    _ => return Err(format!("{at}: unknown matcher {e}")),
                },
            };
            return if ok {
                Ok(())
            } else {
                Err(format!("{at}: expected {e}, got {actual}"))
            };
        }
    }
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            for key in e.keys() {
                if !a.contains_key(key) {
                    return Err(format!("{at}: missing key \"{key}\" in {actual}"));
                }
            }
            for key in a.keys() {
                if !e.contains_key(key) {
                    return Err(format!("{at}: unexpected key \"{key}\" in {actual}"));
                }
            }
            for (key, ev) in e {
                match_json(ev, &a[key], &format!("{at}.{key}"))?;
            }
            Ok(())
        }
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                return Err(format!(
                    "{at}: expected {} elements, got {}: {actual}",
                    e.len(),
                    a.len()
                ));
            }
            for (i, (ev, av)) in e.iter().zip(a).enumerate() {
                match_json(ev, av, &format!("{at}[{i}]"))?;
            }
            Ok(())
        }
        _ if expected == actual => Ok(()),
        _ => Err(format!("{at}: expected {expected}, got {actual}")),
    }
}

/// Replaces `{{name}}` placeholders in every string of `value`.
fn substitute(value: &mut Value, vars: &[(&str, String)]) {
    match value {
        Value::String(s) => {
            for (name, replacement) in vars {
                *s = s.replace(&format!("{{{{{name}}}}}"), replacement);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| substitute(v, vars)),
        Value::Object(map) => map.values_mut().for_each(|v| substitute(v, vars)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Scripted HTTP server
// ---------------------------------------------------------------------------

struct Exchange {
    request: Value,
    response: Value,
}

#[derive(Default)]
struct ServerState {
    queue: VecDeque<Exchange>,
    /// Requests currently being handled; a step is not done until these are.
    in_flight: usize,
    errors: Vec<String>,
}

struct FakeServer {
    port: u16,
    state: Arc<(Mutex<ServerState>, Condvar)>,
}

struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let mut body = Vec::new();
    if let Some(len) = headers.get("content-length") {
        body.resize(len.parse().unwrap_or(0), 0);
        reader.read_exact(&mut body)?;
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
    {
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            let size = usize::from_str_radix(line.trim(), 16).unwrap_or(0);
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk)?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    }
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn match_request(expected: &Value, request: &Request) -> Result<(), String> {
    let method = expected["method"].as_str().unwrap_or("GET");
    if method != request.method || expected["path"].as_str() != Some(request.path.as_str()) {
        return Err(format!(
            "expected {method} {}, got {} {}",
            expected["path"], request.method, request.path
        ));
    }
    if let Some(headers) = expected.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            let actual = request.headers.get(&name.to_ascii_lowercase());
            match (value, actual) {
                (Value::Null, None) => {}
                (Value::String(e), Some(a)) if e == a => {}
                _ => {
                    return Err(format!(
                        "{} {}: header {name}: expected {value}, got {actual:?}",
                        request.method, request.path
                    ))
                }
            }
        }
    }
    if let Some(expected_json) = expected.get("json") {
        let actual: Value = serde_json::from_slice(&request.body).map_err(|e| {
            format!(
                "{} {}: body is not JSON ({e}): {}",
                request.method,
                request.path,
                String::from_utf8_lossy(&request.body)
            )
        })?;
        match_json(expected_json, &actual, "body")
            .map_err(|e| format!("{} {}: {e}", request.method, request.path))?;
    }
    Ok(())
}

fn reason_phrase(status: u64) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn write_response(stream: &mut TcpStream, response: &Value) -> std::io::Result<()> {
    if response["drop_connection"].as_bool() == Some(true) {
        return Ok(());
    }
    let status = response["status"].as_u64().unwrap_or(200);
    let mut headers: Vec<(String, String)> = Vec::new();
    let body: Vec<u8> = if let Some(json) = response.get("json") {
        headers.push(("Content-Type".into(), "application/json".into()));
        serde_json::to_vec(json).unwrap()
    } else if let Some(text) = response["text"].as_str() {
        text.as_bytes().to_vec()
    } else if let Some(file) = response.get("body") {
        read_file_spec(file)
    } else {
        Vec::new()
    };
    if let Some(extra) = response.get("headers").and_then(Value::as_object) {
        for (name, value) in extra {
            headers.push((name.clone(), value.as_str().unwrap_or_default().to_string()));
        }
    }
    if response["omit_content_length"].as_bool() != Some(true) {
        headers.push(("Content-Length".into(), body.len().to_string()));
    }
    headers.push(("Connection".into(), "close".into()));
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    let send = response["close_after_bytes"]
        .as_u64()
        .map_or(body.len(), |n| (n as usize).min(body.len()));
    stream.write_all(&body[..send])?;
    stream.flush()
}

impl FakeServer {
    fn start() -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new((Mutex::new(ServerState::default()), Condvar::new()));
        let thread_state = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let state = thread_state.clone();
                // Taking `in_flight` before the request is parsed would race
                // with `wait_idle`; take it once a connection exists.
                state.0.lock().unwrap().in_flight += 1;
                std::thread::spawn(move || {
                    let response = match read_request(&mut stream) {
                        Ok(request) => {
                            let mut guard = state.0.lock().unwrap();
                            match guard.queue.pop_front() {
                                Some(exchange) => {
                                    match match_request(&exchange.request, &request) {
                                        Ok(()) => Some(exchange.response),
                                        Err(e) => {
                                            guard.errors.push(e);
                                            None
                                        }
                                    }
                                }
                                None => {
                                    guard.errors.push(format!(
                                        "unexpected request: {} {} {}",
                                        request.method,
                                        request.path,
                                        String::from_utf8_lossy(&request.body)
                                    ));
                                    None
                                }
                            }
                        }
                        Err(_) => None,
                    };
                    let response = response.unwrap_or_else(
                        || json!({"status": 500, "text": "fixture runner: request did not match"}),
                    );
                    let _ = write_response(&mut stream, &response);
                    let _ = stream.shutdown(Shutdown::Both);
                    let (lock, cvar) = &*state;
                    lock.lock().unwrap().in_flight -= 1;
                    cvar.notify_all();
                });
            }
        });
        FakeServer { port, state }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn enqueue(&self, exchanges: &[Value]) {
        let mut guard = self.state.0.lock().unwrap();
        for e in exchanges {
            guard.queue.push_back(Exchange {
                request: e["request"].clone(),
                response: e["response"].clone(),
            });
        }
    }

    /// Waits for every queued exchange to be consumed and every connection to
    /// finish, then returns (and clears) the errors seen so far.
    fn wait_idle(&self, timeout: Duration) -> Vec<String> {
        let (lock, cvar) = &*self.state;
        let deadline = Instant::now() + timeout;
        let mut guard = lock.lock().unwrap();
        while !(guard.queue.is_empty() && guard.in_flight == 0) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            guard = cvar.wait_timeout(guard, deadline - now).unwrap().0;
        }
        let mut errors = std::mem::take(&mut guard.errors);
        if !guard.queue.is_empty() {
            let missing: Vec<String> = guard
                .queue
                .drain(..)
                .map(|e| format!("{} {}", e.request["method"], e.request["path"]))
                .collect();
            errors.push(format!("expected requests never arrived: {missing:?}"));
        }
        errors
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Bytes described by a file spec: `{"json": ...}`, `{"text": "..."}`, or
/// `{"file": "<path under fixtures/>", "offset": n, "length": n}`.
fn read_file_spec(spec: &Value) -> Vec<u8> {
    if let Some(json) = spec.get("json") {
        return serde_json::to_vec_pretty(json).unwrap();
    }
    if let Some(text) = spec["text"].as_str() {
        return text.as_bytes().to_vec();
    }
    let rel = spec["file"]
        .as_str()
        .unwrap_or_else(|| panic!("bad file spec {spec}"));
    let bytes = std::fs::read(fixtures_root().join(rel))
        .unwrap_or_else(|e| panic!("reading fixture file {rel}: {e}"));
    let offset = spec["offset"].as_u64().unwrap_or(0) as usize;
    let end = spec["length"]
        .as_u64()
        .map_or(bytes.len(), |n| offset + n as usize);
    bytes[offset..end].to_vec()
}

fn write_files(root: &Path, files: &Map<String, Value>) {
    for (rel, spec) in files {
        let path = root.join(rel);
        if spec["delete"].as_bool() == Some(true) {
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
            continue;
        }
        if spec["dir"].as_bool() == Some(true) {
            std::fs::create_dir_all(&path).unwrap();
            continue;
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, read_file_spec(spec)).unwrap();
    }
}

fn check_files(root: &Path, files: &Map<String, Value>) -> Vec<String> {
    let mut errors = Vec::new();
    for (rel, spec) in files {
        let path = root.join(rel);
        if spec["absent"].as_bool() == Some(true) {
            if path.exists() {
                errors.push(format!("{rel}: expected absent, but it exists"));
            }
            continue;
        }
        if !path.exists() {
            errors.push(format!("{rel}: expected to exist, but it is absent"));
            continue;
        }
        if spec["exists"].as_bool() == Some(true) {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        if let Some(expected) = spec.get("json") {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(actual) => {
                    if let Err(e) = match_json(expected, &actual, rel) {
                        errors.push(e);
                    }
                }
                Err(e) => errors.push(format!(
                    "{rel}: not JSON ({e}): {}",
                    String::from_utf8_lossy(&bytes)
                )),
            }
        }
        if let Some(text) = spec["text"].as_str() {
            if bytes != text.as_bytes() {
                errors.push(format!(
                    "{rel}: expected text {text:?}, got {:?}",
                    String::from_utf8_lossy(&bytes)
                ));
            }
        }
        if let Some(size) = spec["size"].as_u64() {
            if bytes.len() as u64 != size {
                errors.push(format!("{rel}: expected {size} bytes, got {}", bytes.len()));
            }
        }
        if let Some(same_as) = spec.get("same_as") {
            let expected = read_file_spec(same_as);
            if bytes != expected {
                errors.push(format!(
                    "{rel}: contents differ from {same_as} (sha256 {} vs {})",
                    sha256_hex(&bytes),
                    sha256_hex(&expected)
                ));
            }
        }
    }
    errors
}

/// Every regular file under `root`, as sorted `/`-separated relative paths.
fn list_tree(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let rel = path.strip_prefix(root).unwrap();
                let parts: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// The engine's side of the C API
// ---------------------------------------------------------------------------

/// The patch base the engine serves through `FileCallbacks` (used on Apple
/// platforms; elsewhere the updater reads `original_libapp_paths[0]`, which
/// holds the same bytes).
static BASE_BYTES: Mutex<Vec<u8>> = Mutex::new(Vec::new());

extern "C" fn base_open() -> *mut libc::c_void {
    let bytes = BASE_BYTES.lock().unwrap().clone();
    Box::into_raw(Box::new(std::io::Cursor::new(bytes))) as *mut libc::c_void
}

extern "C" fn base_read(handle: *mut libc::c_void, buffer: *mut u8, count: usize) -> usize {
    let cursor = unsafe { &mut *(handle as *mut std::io::Cursor<Vec<u8>>) };
    let out = unsafe { std::slice::from_raw_parts_mut(buffer, count) };
    cursor.read(out).unwrap_or(0)
}

extern "C" fn base_seek(handle: *mut libc::c_void, offset: i64, whence: i32) -> i64 {
    let cursor = unsafe { &mut *(handle as *mut std::io::Cursor<Vec<u8>>) };
    let pos = match whence {
        libc::SEEK_SET => SeekFrom::Start(offset as u64),
        libc::SEEK_CUR => SeekFrom::Current(offset),
        _ => SeekFrom::End(offset),
    };
    cursor.seek(pos).map(|p| p as i64).unwrap_or(-1)
}

extern "C" fn base_close(handle: *mut libc::c_void) {
    drop(unsafe { Box::from_raw(handle as *mut std::io::Cursor<Vec<u8>>) });
}

fn c_string(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn optional_channel(step: &Value) -> Option<CString> {
    step["channel"].as_str().map(c_string)
}

fn channel_ptr(channel: &Option<CString>) -> *const c_char {
    channel.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())
}

struct CaseEnv {
    root: PathBuf,
    libapp: PathBuf,
    server_url: String,
}

fn call_init(env: &CaseEnv, step: &Value, case: &Value) -> Value {
    let release_version = step["release_version"]
        .as_str()
        .or(case["release_version"].as_str())
        .unwrap_or(DEFAULT_RELEASE_VERSION);
    let yaml = step["yaml"]
        .as_str()
        .or(case["yaml"].as_str())
        .unwrap_or(DEFAULT_YAML);
    let release_version = c_string(release_version);
    let yaml = c_string(&yaml.replace("{{server}}", &env.server_url));
    let storage = c_string(env.root.join("storage").to_str().unwrap());
    let cache = c_string(env.root.join("cache").to_str().unwrap());
    let libapp = c_string(env.libapp.to_str().unwrap());
    let libapp_paths = [libapp.as_ptr()];
    let params = engine::AppParameters {
        release_version: release_version.as_ptr(),
        original_libapp_paths: libapp_paths.as_ptr(),
        original_libapp_paths_size: 1,
        app_storage_dir: storage.as_ptr(),
        code_cache_dir: cache.as_ptr(),
    };
    let callbacks = engine::FileCallbacks {
        open: base_open,
        read: base_read,
        seek: base_seek,
        close: base_close,
    };
    Value::Bool(engine::shorebird_init(&params, callbacks, yaml.as_ptr()))
}

/// Runs one step's call and returns what it produced, or `None` for calls
/// with no result.
fn run_call(env: &CaseEnv, step: &Value, case: &Value) -> Result<Option<Value>, String> {
    let call = step["call"].as_str().unwrap_or_default();
    Ok(match call {
        "init" => Some(call_init(env, step, case)),
        "restart" => {
            updater::testing_reset_config();
            None
        }
        "report_launch_start" => {
            engine::shorebird_report_launch_start();
            None
        }
        "report_launch_success" => {
            engine::shorebird_report_launch_success();
            None
        }
        "report_launch_failure" => {
            engine::shorebird_report_launch_failure();
            None
        }
        "validate_next_boot_patch" => {
            engine::shorebird_validate_next_boot_patch();
            None
        }
        "should_auto_update" => Some(Value::Bool(engine::shorebird_should_auto_update())),
        "next_boot_patch_path" => {
            let ptr = engine::shorebird_next_boot_patch_path();
            if ptr.is_null() {
                Some(Value::Null)
            } else {
                let path = unsafe { CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { engine::shorebird_free_string(ptr) };
                // Report it relative to the case root, `/`-separated.
                let rel = Path::new(&path)
                    .strip_prefix(&env.root)
                    .map_err(|_| format!("next_boot_patch_path {path} is outside the case root"))?;
                let parts: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                Some(Value::String(parts.join("/")))
            }
        }
        "next_boot_patch_number" => Some(json!(dart::shorebird_next_boot_patch_number())),
        "current_boot_patch_number" => Some(json!(dart::shorebird_current_boot_patch_number())),
        "check_for_downloadable_update" => {
            let channel = optional_channel(step);
            Some(Value::Bool(dart::shorebird_check_for_downloadable_update(
                channel_ptr(&channel),
            )))
        }
        "update" => {
            let channel = optional_channel(step);
            let result = dart::shorebird_update_with_result(channel_ptr(&channel));
            let (status, message) = unsafe {
                let r = &*result;
                let message = if r.message.is_null() {
                    Value::Null
                } else {
                    Value::String(CStr::from_ptr(r.message).to_string_lossy().into_owned())
                };
                (r.status, message)
            };
            unsafe { dart::shorebird_free_update_result(result as *mut dart::UpdateResult) };
            Some(json!({"status": status, "message": message}))
        }
        "" => None, // A step that only writes or checks files.
        other => return Err(format!("unknown call \"{other}\"")),
    })
}

fn run_case(case_path: &Path) -> Result<(), Vec<String>> {
    let raw = std::fs::read_to_string(case_path).unwrap();
    let mut case: Value = serde_json::from_str(&raw)
        .map_err(|e| vec![format!("{}: invalid JSON: {e}", case_path.display())])?;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let server = FakeServer::start();
    let public_key = std::fs::read_to_string(fixtures_root().join("keys/fixture_public_key.b64"))
        .unwrap()
        .trim()
        .to_string();
    substitute(
        &mut case,
        &[("server", server.base_url()), ("public_key", public_key)],
    );

    let base = case
        .get("base")
        .map(read_file_spec)
        .unwrap_or_else(|| b"base release libapp".to_vec());
    let libapp = root.join("app").join("libapp.so");
    std::fs::create_dir_all(libapp.parent().unwrap()).unwrap();
    std::fs::write(&libapp, &base).unwrap();
    *BASE_BYTES.lock().unwrap() = base;
    let env = CaseEnv {
        root: root.clone(),
        libapp,
        server_url: server.base_url(),
    };

    if let Some(files) = case.get("files").and_then(Value::as_object) {
        write_files(&root, files);
    }

    updater::testing_reset_config();
    let mut errors = Vec::new();
    let steps = case["steps"].as_array().cloned().unwrap_or_default();
    for (i, step) in steps.iter().enumerate() {
        let label = format!(
            "step {i} ({})",
            step["call"].as_str().unwrap_or("files only")
        );
        if let Some(files) = step.get("write_files").and_then(Value::as_object) {
            write_files(&root, files);
        }
        if let Some(http) = step.get("http").and_then(Value::as_array) {
            server.enqueue(http);
        }
        match run_call(&env, step, &case) {
            Ok(Some(actual)) => {
                if let Some(expected) = step.get("expect") {
                    if let Err(e) = match_json(expected, &actual, "result") {
                        errors.push(format!("{label}: {e}"));
                    }
                }
            }
            Ok(None) => {}
            Err(e) => errors.push(format!("{label}: {e}")),
        }
        for e in server.wait_idle(REQUEST_TIMEOUT) {
            errors.push(format!("{label}: {e}"));
        }
        if let Some(files) = step.get("expect_files").and_then(Value::as_object) {
            for e in check_files(&root, files) {
                errors.push(format!("{label}: {e}"));
            }
        }
        if let Some(tree) = step.get("expect_tree") {
            let actual = Value::from(list_tree(&root));
            if let Err(e) = match_json(tree, &actual, "tree") {
                errors.push(format!("{label}: {e}"));
            }
        }
        if !errors.is_empty() {
            break;
        }
    }
    std::thread::sleep(QUIET_PERIOD);
    for e in server.wait_idle(Duration::ZERO) {
        errors.push(format!("after last step: {e}"));
    }
    updater::testing_reset_config();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Turns each `fixtures/bipatch/<name>/` triple into a case: the server
/// offers `patch.bin`, the base release is `base.bin`, and the installed
/// patch must equal `new.bin`.
fn bipatch_case(dir: &Path) -> Value {
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let new = std::fs::read(dir.join("new.bin")).unwrap();
    let rel = |f: &str| format!("bipatch/{name}/{f}");
    json!({
        "description": format!("bipatch golden {name}"),
        "base": {"file": rel("base.bin")},
        "files": {
            "storage/state.json": {"json": {
                "client_id": "00000000-0000-4000-8000-000000000001",
                "release_version": DEFAULT_RELEASE_VERSION,
                "queued_events": []
            }}
        },
        "steps": [
            {"call": "init", "expect": true},
            {
                "call": "update",
                "http": [
                    {
                        "request": {"method": "POST", "path": "/api/v1/patches/check"},
                        "response": {"status": 200, "json": {
                            "patch_available": true,
                            "patch": {
                                "number": 1,
                                "hash": sha256_hex(&new),
                                "download_url": "{{server}}/patch.bin"
                            }
                        }}
                    },
                    {
                        "request": {"method": "GET", "path": "/patch.bin"},
                        "response": {"status": 200, "body": {"file": rel("patch.bin")}}
                    },
                    {
                        "request": {"method": "POST", "path": "/api/v1/patches/events"},
                        "response": {"status": 200, "text": ""}
                    }
                ],
                "expect": {"status": 1, "message": "Update installed"},
                "expect_files": {
                    "storage/patches/1/dlc.vmcode": {"same_as": {"file": rel("new.bin")}}
                }
            }
        ]
    })
}

fn collect_cases(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_cases(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

/// One test for the whole corpus: the updater's config is process-global, so
/// cases must run one after another.
#[test]
fn fixture_corpus() {
    let root = fixtures_root();
    let filter = std::env::var("FIXTURE_FILTER").unwrap_or_default();
    let mut failures = Vec::new();
    let mut ran = 0;

    let mut case_files = Vec::new();
    collect_cases(&root.join("cases"), &mut case_files);
    for path in case_files {
        let name = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !name.contains(&filter) {
            continue;
        }
        ran += 1;
        if let Err(errors) = run_case(&path) {
            failures.push(format!("{name}:\n    {}", errors.join("\n    ")));
        }
    }

    let mut golden_dirs: Vec<PathBuf> = std::fs::read_dir(root.join("bipatch"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    golden_dirs.sort();
    for dir in golden_dirs {
        let name = format!("bipatch/{}", dir.file_name().unwrap().to_string_lossy());
        if !name.contains(&filter) {
            continue;
        }
        ran += 1;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), bipatch_case(&dir).to_string()).unwrap();
        if let Err(errors) = run_case(tmp.path()) {
            failures.push(format!("{name}:\n    {}", errors.join("\n    ")));
        }
    }

    assert!(ran > 0, "no fixture cases matched filter {filter:?}");
    assert!(
        failures.is_empty(),
        "{} of {ran} fixture cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("{ran} fixture cases passed");
}
