//! Engine callbacks for tests, which run without an engine: HTTP through
//! `ureq` (plain `http://` only, which is all the test servers speak) and
//! signature checks through `ring`. Neither crate is part of a production
//! build: they are dev-dependencies, and optional dependencies enabled only
//! by the `test-hooks` feature, for the Dart integration tests.

use crate::c_api::engine::{
    EngineCallbacks, HttpHeader, HttpRequest, HttpResponseCallbacks, SHOREBIRD_HTTP_ABORTED,
    SHOREBIRD_HTTP_INVALID_REQUEST, SHOREBIRD_HTTP_OK, SHOREBIRD_HTTP_PROTOCOL_ERROR,
    SHOREBIRD_HTTP_TRANSPORT_ERROR,
};
use std::ffi::{CStr, CString};
use std::io::Read;

pub fn callbacks() -> EngineCallbacks {
    EngineCallbacks {
        http_request: Some(http_request),
        verify_signature: Some(verify_signature),
    }
}

fn c_str(ptr: *const libc::c_char) -> Option<&'static str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

extern "C" fn http_request(
    request: *const HttpRequest,
    callbacks: *const HttpResponseCallbacks,
) -> i32 {
    let request = unsafe { &*request };
    let callbacks = unsafe { &*callbacks };
    let (Some(method), Some(url)) = (c_str(request.method), c_str(request.url)) else {
        return SHOREBIRD_HTTP_INVALID_REQUEST;
    };
    let headers: &[HttpHeader] = if request.header_count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(request.headers, request.header_count) }
    };
    let body: &[u8] = if request.body_length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(request.body, request.body_length) }
    };

    // Statuses are the caller's to judge, so ureq must not turn 4xx/5xx into
    // errors.
    let timeout = (request.timeout_ms > 0)
        .then(|| std::time::Duration::from_millis(request.timeout_ms.into()));
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(timeout)
        .build()
        .new_agent();
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    for header in headers {
        let (Some(name), Some(value)) = (c_str(header.name), c_str(header.value)) else {
            return SHOREBIRD_HTTP_INVALID_REQUEST;
        };
        builder = builder.header(name, value);
    }
    let Ok(http_request) = builder.body(body.to_vec()) else {
        return SHOREBIRD_HTTP_INVALID_REQUEST;
    };
    let response = match agent.run(http_request) {
        Ok(response) => response,
        Err(e) => {
            if let Ok(message) = CString::new(e.to_string()) {
                (callbacks.on_error)(callbacks.context, message.as_ptr());
            }
            return match e {
                ureq::Error::HostNotFound
                | ureq::Error::ConnectionFailed
                | ureq::Error::Io(_)
                | ureq::Error::Timeout(_) => SHOREBIRD_HTTP_TRANSPORT_ERROR,
                _ => SHOREBIRD_HTTP_PROTOCOL_ERROR,
            };
        }
    };

    let header_strings: Vec<(CString, CString)> = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            Some((
                CString::new(name.as_str()).ok()?,
                CString::new(value.as_bytes()).ok()?,
            ))
        })
        .collect();
    let c_headers: Vec<HttpHeader> = header_strings
        .iter()
        .map(|(name, value)| HttpHeader {
            name: name.as_ptr(),
            value: value.as_ptr(),
        })
        .collect();
    if !(callbacks.on_response)(
        callbacks.context,
        response.status().as_u16(),
        c_headers.as_ptr(),
        c_headers.len(),
    ) {
        return SHOREBIRD_HTTP_ABORTED;
    }
    let mut reader = response.into_body().into_reader();
    let mut buffer = [0u8; 8192];
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(0) => return SHOREBIRD_HTTP_OK,
            Ok(n) => n,
            Err(_) => return SHOREBIRD_HTTP_TRANSPORT_ERROR,
        };
        if !(callbacks.on_body)(callbacks.context, buffer.as_ptr(), n) {
            return SHOREBIRD_HTTP_ABORTED;
        }
    }
}

extern "C" fn verify_signature(
    public_key: *const u8,
    public_key_length: usize,
    message: *const u8,
    message_length: usize,
    signature: *const u8,
    signature_length: usize,
) -> bool {
    let slice = |ptr: *const u8, len: usize| -> &[u8] {
        if len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(ptr, len) }
        }
    };
    let key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        slice(public_key, public_key_length),
    );
    key.verify(
        slice(message, message_length),
        slice(signature, signature_length),
    )
    .is_ok()
}
