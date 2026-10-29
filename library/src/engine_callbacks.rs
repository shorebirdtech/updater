//! The services the engine registers through `shorebird_set_engine_callbacks`:
//! patch signature verification on the engine's BoringSSL, and, on iOS, macOS
//! and Android, HTTP on the platform's HTTP stack.
//!
//! This module holds the registered callbacks and wraps them in safe Rust
//! functions. The C types live in `c_api::engine` so cbindgen emits them.

use std::sync::{PoisonError, RwLock};

use crate::c_api::engine::EngineCallbacks;
#[cfg(any(engine_http, test))]
pub use http::*;

static CALLBACKS: RwLock<Option<EngineCallbacks>> = RwLock::new(None);

pub fn set(callbacks: EngineCallbacks) {
    *CALLBACKS.write().unwrap_or_else(PoisonError::into_inner) = Some(callbacks);
}

/// The registered callbacks. Unit tests that register none get the test
/// implementations, so every test that reaches the network or a signature
/// check runs through the same C boundary the engine uses.
fn get() -> Option<EngineCallbacks> {
    let registered = *CALLBACKS.read().unwrap_or_else(PoisonError::into_inner);
    #[cfg(test)]
    if registered.is_none() {
        return Some(crate::test_engine_callbacks::callbacks());
    }
    registered
}

/// HTTP through the engine, used only where the engine's client is the
/// platform's HTTP stack (see `crate::http`).
#[cfg(any(engine_http, test))]
mod http {
    use std::ffi::{c_void, CStr, CString};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use super::get;
    use crate::c_api::engine::{
        EngineCallbacks, HttpHeader, HttpRequest, HttpResponseCallbacks,
        SHOREBIRD_HTTP_INVALID_REQUEST, SHOREBIRD_HTTP_OK, SHOREBIRD_HTTP_PROTOCOL_ERROR,
        SHOREBIRD_HTTP_TLS_ERROR, SHOREBIRD_HTTP_TRANSPORT_ERROR,
    };
    use crate::http::{Header, Request, ResponseHandler};

    /// How an HTTP request failed before a whole response was delivered.
    #[derive(Debug)]
    pub enum HttpError {
        /// The server could not be reached, or the connection failed mid-way.
        /// Each failure carries the engine's reason, if it gave one.
        Transport(Option<String>),
        /// The TLS handshake failed.
        Tls(Option<String>),
        /// The response was not valid HTTP, or redirected too many times.
        Protocol(Option<String>),
        /// The request was unusable (e.g. a malformed URL).
        InvalidRequest,
        /// A response handler returned this error, which stopped the request.
        Handler(anyhow::Error),
    }

    /// Sends a request through the engine's HTTP client, delivering the response
    /// to `handler`. Blocks until the response is complete.
    pub fn http_request(
        request: &Request,
        handler: &mut dyn ResponseHandler,
    ) -> Result<(), HttpError> {
        http_request_with(get(), request, handler)
    }

    struct HandlerContext<'a> {
        handler: &'a mut dyn ResponseHandler,
        error: Option<anyhow::Error>,
        /// Whether `on_response` was delivered; body bytes before it, a second
        /// one, or success without it, break the contract and fail the request.
        responded: bool,
        /// The reason the engine passed to `on_error`.
        engine_error: Option<String>,
    }

    impl HandlerContext<'_> {
        /// Runs `f`, recording an error or a panic so the request stops. A panic
        /// must not unwind into the engine's C++ frames.
        fn run(&mut self, f: impl FnOnce(&mut dyn ResponseHandler) -> anyhow::Result<()>) -> bool {
            if self.error.is_some() {
                return false;
            }
            let handler = &mut *self.handler;
            let result = catch_unwind(AssertUnwindSafe(|| f(handler)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("HTTP response handler panicked")));
            match result {
                Ok(()) => true,
                Err(e) => {
                    self.error = Some(e);
                    false
                }
            }
        }
    }

    /// Copies C strings the engine passed in; invalid UTF-8 is replaced rather
    /// than failing, since header values are only ever parsed as ASCII.
    ///
    /// # Safety
    ///
    /// `ptr` must be null or a valid NUL-terminated string.
    unsafe fn lossy_string(ptr: *const libc::c_char) -> String {
        if ptr.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }

    extern "C" fn on_response_trampoline(
        context: *mut c_void,
        status: u16,
        headers: *const HttpHeader,
        header_count: usize,
    ) -> bool {
        // SAFETY: `context` is the `HandlerContext` `http_request_with` passed in,
        // which outlives the engine's `http_request` call that invokes this.
        let context = unsafe { &mut *(context as *mut HandlerContext) };
        let headers: Vec<Header> = if headers.is_null() || header_count == 0 {
            Vec::new()
        } else {
            // SAFETY: the engine passes `header_count` valid headers.
            unsafe { std::slice::from_raw_parts(headers, header_count) }
                .iter()
                .map(|h| Header {
                    name: unsafe { lossy_string(h.name) },
                    value: unsafe { lossy_string(h.value) },
                })
                .collect()
        };
        if context.responded {
            context.error = Some(anyhow::anyhow!("HTTP response delivered twice"));
            return false;
        }
        context.responded = true;
        context.run(|handler| handler.on_response(status, &headers))
    }

    extern "C" fn on_error_trampoline(context: *mut c_void, message: *const libc::c_char) {
        // SAFETY: as in `on_response_trampoline`.
        let context = unsafe { &mut *(context as *mut HandlerContext) };
        // SAFETY: the engine passes null or a NUL-terminated string.
        let message = unsafe { lossy_string(message) };
        if context.engine_error.is_none() && !message.is_empty() {
            context.engine_error = Some(message);
        }
    }

    extern "C" fn on_body_trampoline(context: *mut c_void, data: *const u8, length: usize) -> bool {
        // SAFETY: as in `on_response_trampoline`.
        let context = unsafe { &mut *(context as *mut HandlerContext) };
        if !context.responded {
            context.error = Some(anyhow::anyhow!(
                "HTTP body delivered before the response status"
            ));
            return false;
        }
        if length == 0 {
            return true;
        }
        if data.is_null() {
            context.error = Some(anyhow::anyhow!("HTTP body callback passed null data"));
            return false;
        }
        // SAFETY: the engine passes `length` readable bytes at `data`.
        let data = unsafe { std::slice::from_raw_parts(data, length) };
        context.run(|handler| handler.on_body(data))
    }

    pub(super) fn http_request_with(
        callbacks: Option<EngineCallbacks>,
        request: &Request,
        handler: &mut dyn ResponseHandler,
    ) -> Result<(), HttpError> {
        let Request {
            method,
            url,
            headers,
            body,
            timeout,
        } = *request;
        let Some(http_request) = callbacks.and_then(|c| c.http_request) else {
            shorebird_error!(
                "No HTTP client registered; call shorebird_set_engine_callbacks first"
            );
            return Err(HttpError::Transport(Some(
                "no HTTP client registered".to_string(),
            )));
        };
        let to_c = |s: &str| CString::new(s).map_err(|_| HttpError::InvalidRequest);
        let c_method = to_c(method)?;
        let c_url = to_c(url)?;
        let c_header_strings = headers
            .iter()
            .map(|(name, value)| Ok((to_c(name)?, to_c(value)?)))
            .collect::<Result<Vec<_>, HttpError>>()?;
        let c_headers: Vec<HttpHeader> = c_header_strings
            .iter()
            .map(|(name, value)| HttpHeader {
                name: name.as_ptr(),
                value: value.as_ptr(),
            })
            .collect();
        // A limit too long for a u32 of milliseconds (49 days) is no limit.
        let timeout_ms = timeout.map_or(0, |t| {
            u32::try_from(t.as_millis()).map_or(0, |ms| ms.max(1))
        });
        let request = HttpRequest {
            method: c_method.as_ptr(),
            url: c_url.as_ptr(),
            headers: c_headers.as_ptr(),
            header_count: c_headers.len(),
            body: if body.is_empty() {
                std::ptr::null()
            } else {
                body.as_ptr()
            },
            body_length: body.len(),
            timeout_ms,
        };
        let mut context = HandlerContext {
            handler,
            error: None,
            responded: false,
            engine_error: None,
        };
        let response_callbacks = HttpResponseCallbacks {
            context: &mut context as *mut HandlerContext as *mut c_void,
            on_response: on_response_trampoline,
            on_body: on_body_trampoline,
            on_error: on_error_trampoline,
        };
        let code = http_request(&request, &response_callbacks);
        // A handler error is the most specific explanation, whatever code the
        // engine returned after the handler stopped the request.
        if let Some(error) = context.error {
            return Err(HttpError::Handler(error));
        }
        let reason = context.engine_error;
        match code {
            SHOREBIRD_HTTP_OK if context.responded => Ok(()),
            SHOREBIRD_HTTP_OK => Err(HttpError::Protocol(Some(
                "the client reported success without a response".to_string(),
            ))),
            SHOREBIRD_HTTP_TRANSPORT_ERROR => Err(HttpError::Transport(reason)),
            SHOREBIRD_HTTP_TLS_ERROR => Err(HttpError::Tls(reason)),
            SHOREBIRD_HTTP_PROTOCOL_ERROR => Err(HttpError::Protocol(reason)),
            SHOREBIRD_HTTP_INVALID_REQUEST => Err(HttpError::InvalidRequest),
            // SHOREBIRD_HTTP_ABORTED without a recorded handler error, or a code
            // this updater does not know: treat as a failed transport.
            _ => Err(HttpError::Transport(reason)),
        }
    }
}

/// Whether the engine registered a signature verifier.
pub fn has_signature_verifier() -> bool {
    get().is_some_and(|c| c.verify_signature.is_some())
}

/// Verifies an RSASSA-PKCS1-v1_5 SHA-256 signature with the engine's
/// verifier. `None` when the engine registered no verifier.
pub fn verify_signature(public_key_der: &[u8], message: &[u8], signature: &[u8]) -> Option<bool> {
    verify_signature_with(get(), public_key_der, message, signature)
}

fn verify_signature_with(
    callbacks: Option<EngineCallbacks>,
    public_key_der: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Option<bool> {
    let verify_signature = callbacks?.verify_signature?;
    Some(verify_signature(
        public_key_der.as_ptr(),
        public_key_der.len(),
        message.as_ptr(),
        message.len(),
        signature.as_ptr(),
        signature.len(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::c_api::engine::{
        HttpHeader, HttpRequest, HttpResponseCallbacks, SHOREBIRD_HTTP_ABORTED, SHOREBIRD_HTTP_OK,
        SHOREBIRD_HTTP_PROTOCOL_ERROR, SHOREBIRD_HTTP_TLS_ERROR, SHOREBIRD_HTTP_TRANSPORT_ERROR,
    };
    use crate::http::{Header, Request, ResponseHandler};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    fn send(
        callbacks: Option<EngineCallbacks>,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        handler: &mut dyn ResponseHandler,
    ) -> Result<(), HttpError> {
        send_with_timeout(callbacks, method, url, headers, body, None, handler)
    }

    fn send_with_timeout(
        callbacks: Option<EngineCallbacks>,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        timeout: Option<Duration>,
        handler: &mut dyn ResponseHandler,
    ) -> Result<(), HttpError> {
        let request = Request {
            method,
            url,
            headers,
            body,
            timeout,
        };
        http::http_request_with(callbacks, &request, handler)
    }

    type HttpRequestFn = extern "C" fn(*const HttpRequest, *const HttpResponseCallbacks) -> i32;

    struct Recorder {
        status: Option<u16>,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        fail_on_body: bool,
        panic_on_response: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Recorder {
                status: None,
                headers: Vec::new(),
                body: Vec::new(),
                fail_on_body: false,
                panic_on_response: false,
            }
        }
    }

    impl ResponseHandler for Recorder {
        fn on_response(&mut self, status: u16, headers: &[Header]) -> anyhow::Result<()> {
            if self.panic_on_response {
                panic!("handler panic");
            }
            self.status = Some(status);
            self.headers = headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect();
            Ok(())
        }
        fn on_body(&mut self, data: &[u8]) -> anyhow::Result<()> {
            if self.fail_on_body {
                anyhow::bail!("disk full");
            }
            self.body.extend_from_slice(data);
            Ok(())
        }
    }

    fn with_http(http_request: HttpRequestFn) -> Option<EngineCallbacks> {
        Some(EngineCallbacks {
            http_request: Some(http_request),
            ..crate::test_engine_callbacks::callbacks()
        })
    }

    // Echoes the request back: status 201, the method and URL as headers, and
    // the request body in two chunks.
    extern "C" fn echo(
        request: *const HttpRequest,
        callbacks: *const HttpResponseCallbacks,
    ) -> i32 {
        let request = unsafe { &*request };
        let callbacks = unsafe { &*callbacks };
        let mut response_headers = vec![
            HttpHeader {
                name: c"Method".as_ptr(),
                value: request.method,
            },
            HttpHeader {
                name: c"Url".as_ptr(),
                value: request.url,
            },
        ];
        let request_headers =
            unsafe { std::slice::from_raw_parts(request.headers, request.header_count) };
        response_headers.extend_from_slice(request_headers);
        if !(callbacks.on_response)(
            callbacks.context,
            201,
            response_headers.as_ptr(),
            response_headers.len(),
        ) {
            return SHOREBIRD_HTTP_ABORTED;
        }
        let body: &[u8] = if request.body_length == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(request.body, request.body_length) }
        };
        let (first, second) = body.split_at(body.len() / 2);
        for chunk in [first, second] {
            if !(callbacks.on_body)(callbacks.context, chunk.as_ptr(), chunk.len()) {
                return SHOREBIRD_HTTP_ABORTED;
            }
        }
        SHOREBIRD_HTTP_OK
    }

    #[test]
    fn delivers_request_and_response_across_the_c_boundary() {
        let mut recorder = Recorder::new();
        let result = send(
            with_http(echo),
            "POST",
            "https://example.com/x",
            &[("Content-Type", "application/json")],
            b"{\"a\":1}",
            &mut recorder,
        );
        assert!(result.is_ok());
        assert_eq!(recorder.status, Some(201));
        assert_eq!(
            recorder.headers,
            vec![
                ("Method".to_string(), "POST".to_string()),
                ("Url".to_string(), "https://example.com/x".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ]
        );
        assert_eq!(recorder.body, b"{\"a\":1}");
    }

    #[test]
    fn handler_error_stops_the_request_and_is_returned() {
        let mut recorder = Recorder::new();
        recorder.fail_on_body = true;
        let result = send(with_http(echo), "POST", "u", &[], b"body", &mut recorder);
        match result {
            Err(HttpError::Handler(e)) => assert_eq!(e.to_string(), "disk full"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn handler_panic_does_not_unwind_into_the_engine() {
        let mut recorder = Recorder::new();
        recorder.panic_on_response = true;
        let result = send(with_http(echo), "GET", "u", &[], b"", &mut recorder);
        match result {
            Err(HttpError::Handler(e)) => {
                assert_eq!(e.to_string(), "HTTP response handler panicked")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn no_registered_client_is_a_transport_error() {
        let mut recorder = Recorder::new();
        let result = send(None, "GET", "u", &[], b"", &mut recorder);
        assert!(matches!(result, Err(HttpError::Transport(_))));
        assert_eq!(recorder.status, None);
    }

    #[test]
    fn interior_nul_is_an_invalid_request() {
        let mut recorder = Recorder::new();
        let result = send(with_http(echo), "GET", "a\0b", &[], b"", &mut recorder);
        assert!(matches!(result, Err(HttpError::InvalidRequest)));
    }

    #[test]
    fn maps_engine_error_codes() {
        extern "C" fn tls(_: *const HttpRequest, _: *const HttpResponseCallbacks) -> i32 {
            SHOREBIRD_HTTP_TLS_ERROR
        }
        extern "C" fn protocol(_: *const HttpRequest, _: *const HttpResponseCallbacks) -> i32 {
            SHOREBIRD_HTTP_PROTOCOL_ERROR
        }
        extern "C" fn unknown(_: *const HttpRequest, _: *const HttpResponseCallbacks) -> i32 {
            99
        }
        let mut recorder = Recorder::new();
        let mut send_to = |f| send(with_http(f), "GET", "u", &[], b"", &mut recorder);
        assert!(matches!(send_to(tls), Err(HttpError::Tls(_))));
        assert!(matches!(send_to(protocol), Err(HttpError::Protocol(_))));
        assert!(matches!(send_to(unknown), Err(HttpError::Transport(_))));
    }

    #[test]
    fn verify_without_registered_verifier_is_none() {
        assert_eq!(verify_signature_with(None, b"k", b"m", b"s"), None);
        let http_only = EngineCallbacks {
            verify_signature: None,
            ..crate::test_engine_callbacks::callbacks()
        };
        assert_eq!(
            verify_signature_with(Some(http_only), b"k", b"m", b"s"),
            None
        );
    }

    #[test]
    fn null_http_request_is_a_transport_error() {
        let verify_only = EngineCallbacks {
            http_request: None,
            ..crate::test_engine_callbacks::callbacks()
        };
        let mut recorder = Recorder::new();
        let result = send(Some(verify_only), "GET", "u", &[], b"", &mut recorder);
        assert!(matches!(result, Err(HttpError::Transport(_))));
    }

    // Reports success without ever delivering a response.
    extern "C" fn silent(_: *const HttpRequest, _: *const HttpResponseCallbacks) -> i32 {
        SHOREBIRD_HTTP_OK
    }

    // Delivers body bytes before the status.
    extern "C" fn body_first(
        _: *const HttpRequest,
        callbacks: *const HttpResponseCallbacks,
    ) -> i32 {
        let callbacks = unsafe { &*callbacks };
        let data = b"x";
        if !(callbacks.on_body)(callbacks.context, data.as_ptr(), data.len()) {
            return SHOREBIRD_HTTP_ABORTED;
        }
        SHOREBIRD_HTTP_OK
    }

    #[test]
    fn ok_without_a_response_is_a_protocol_error() {
        let mut recorder = Recorder::new();
        let result = send(with_http(silent), "GET", "u", &[], b"", &mut recorder);
        assert!(matches!(result, Err(HttpError::Protocol(_))));
    }

    #[test]
    fn body_before_response_fails_the_request() {
        let mut recorder = Recorder::new();
        let result = send(with_http(body_first), "GET", "u", &[], b"", &mut recorder);
        match result {
            Err(HttpError::Handler(e)) => {
                assert_eq!(
                    e.to_string(),
                    "HTTP body delivered before the response status"
                )
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(recorder.body.is_empty());
    }

    #[test]
    fn empty_request_body_echoes_nothing() {
        let mut recorder = Recorder::new();
        let result = send(with_http(echo), "GET", "u", &[], b"", &mut recorder);
        assert!(result.is_ok());
        assert!(recorder.body.is_empty());
    }

    static LAST_TIMEOUT_MS: AtomicU32 = AtomicU32::new(u32::MAX);

    // Records the request's timeout and answers 204.
    extern "C" fn record_timeout(
        request: *const HttpRequest,
        callbacks: *const HttpResponseCallbacks,
    ) -> i32 {
        let request = unsafe { &*request };
        let callbacks = unsafe { &*callbacks };
        LAST_TIMEOUT_MS.store(request.timeout_ms, Ordering::SeqCst);
        if !(callbacks.on_response)(callbacks.context, 204, std::ptr::null(), 0) {
            return SHOREBIRD_HTTP_ABORTED;
        }
        SHOREBIRD_HTTP_OK
    }

    #[test]
    fn passes_the_timeout_in_milliseconds() {
        let mut recorder = Recorder::new();
        let mut send_timeout = |timeout| {
            send_with_timeout(
                with_http(record_timeout),
                "GET",
                "u",
                &[],
                b"",
                timeout,
                &mut recorder,
            )
            .unwrap();
            LAST_TIMEOUT_MS.load(Ordering::SeqCst)
        };
        assert_eq!(send_timeout(None), 0);
        assert_eq!(send_timeout(Some(Duration::from_secs(30))), 30_000);
        // Rounding must not turn a tiny limit into none.
        assert_eq!(send_timeout(Some(Duration::from_micros(10))), 1);
        assert_eq!(send_timeout(Some(Duration::from_secs(u64::MAX))), 0);
    }

    // Answers twice.
    extern "C" fn two_responses(
        _: *const HttpRequest,
        callbacks: *const HttpResponseCallbacks,
    ) -> i32 {
        let callbacks = unsafe { &*callbacks };
        for status in [200, 206] {
            if !(callbacks.on_response)(callbacks.context, status, std::ptr::null(), 0) {
                return SHOREBIRD_HTTP_ABORTED;
            }
        }
        SHOREBIRD_HTTP_OK
    }

    #[test]
    fn second_response_fails_the_request() {
        let mut recorder = Recorder::new();
        let result = send(
            with_http(two_responses),
            "GET",
            "u",
            &[],
            b"",
            &mut recorder,
        );
        match result {
            Err(HttpError::Handler(e)) => {
                assert_eq!(e.to_string(), "HTTP response delivered twice")
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(recorder.status, Some(200));
    }

    // Fails with a reason.
    extern "C" fn refused(_: *const HttpRequest, callbacks: *const HttpResponseCallbacks) -> i32 {
        let callbacks = unsafe { &*callbacks };
        (callbacks.on_error)(callbacks.context, c"Connection refused".as_ptr());
        SHOREBIRD_HTTP_TRANSPORT_ERROR
    }

    #[test]
    fn engine_reason_is_kept() {
        let mut recorder = Recorder::new();
        let result = send(with_http(refused), "GET", "u", &[], b"", &mut recorder);
        match result {
            Err(HttpError::Transport(Some(reason))) => assert_eq!(reason, "Connection refused"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
