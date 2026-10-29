//! The HTTP transport under `network`. On iOS, macOS and Android requests go
//! through the engine's client, which uses the platform's HTTP stack
//! (registered with `shorebird_set_engine_callbacks`). Elsewhere they go
//! through `ureq` and rustls, checking certificates with the platform's
//! verifier.

use std::time::Duration;

use crate::network::NetworkError;

/// A request for a [`SendFn`].
pub struct Request<'a> {
    /// "GET" or "POST".
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
    /// The longest the whole request may take, or `None` for no limit.
    pub timeout: Option<Duration>,
}

/// A response header.
pub struct Header {
    pub name: String,
    pub value: String,
}

/// Finds the first header named `name`, ignoring ASCII case.
pub fn find_header<'a>(headers: &'a [Header], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// Receives a response from a [`SendFn`].
pub trait ResponseHandler {
    /// The final status and headers, before any body bytes.
    fn on_response(&mut self, status: u16, headers: &[Header]) -> anyhow::Result<()>;
    /// The next run of body bytes.
    fn on_body(&mut self, data: &[u8]) -> anyhow::Result<()>;
}

/// Sends a request and delivers the response, whatever its status, to the
/// handler, blocking until it is complete. A handler error stops the request
/// and is returned as is. Failing to reach the server, or the connection
/// failing mid-response, is a [`NetworkError::Transport`] with the cause
/// attached.
pub type SendFn = fn(&Request, &mut dyn ResponseHandler) -> anyhow::Result<()>;

#[cfg(engine_http)]
pub const PLATFORM_SEND: SendFn = engine_send;
#[cfg(not(engine_http))]
pub const PLATFORM_SEND: SendFn = ureq_send;

/// Sends through the engine's HTTP client.
#[cfg(any(engine_http, test))]
pub fn engine_send(request: &Request, handler: &mut dyn ResponseHandler) -> anyhow::Result<()> {
    crate::engine_callbacks::http_request(request, handler)
        .map_err(|e| engine_error(e, request.url))
}

#[cfg(any(engine_http, test))]
fn engine_error(error: crate::engine_callbacks::HttpError, url: &str) -> anyhow::Error {
    use crate::engine_callbacks::HttpError;
    match error {
        HttpError::Handler(e) => e,
        HttpError::Transport => NetworkError::Transport.into(),
        // An untrusted certificate (a TLS-inspecting proxy, say) means the
        // server couldn't be reached, as ureq reports it.
        HttpError::Tls => anyhow::anyhow!("TLS handshake failed").context(NetworkError::Transport),
        HttpError::Protocol => anyhow::anyhow!("Invalid HTTP response"),
        HttpError::InvalidRequest => anyhow::anyhow!("Invalid HTTP request for URL: {url}"),
    }
}

/// Sends through `ureq`.
#[cfg(any(not(engine_http), test))]
pub fn ureq_send(request: &Request, handler: &mut dyn ResponseHandler) -> anyhow::Result<()> {
    use std::io::{ErrorKind, Read};
    use ureq::tls::{RootCerts, TlsConfig};

    // Statuses are the caller's to judge, so ureq must not turn 4xx/5xx into
    // errors.
    static AGENT: once_cell::sync::Lazy<ureq::Agent> = once_cell::sync::Lazy::new(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .new_agent()
    });

    let mut builder = ureq::http::Request::builder()
        .method(request.method)
        .uri(request.url);
    for (name, value) in request.headers {
        builder = builder.header(*name, *value);
    }
    let http_request = builder
        .body(request.body)
        .map_err(|e| anyhow::anyhow!("Invalid HTTP request for URL {}: {e}", request.url))?;
    let http_request = AGENT
        .configure_request(http_request)
        .timeout_global(request.timeout)
        .build();
    let response = match AGENT.run(http_request) {
        Ok(response) => response,
        // Keep ureq's error as the cause, so logs and the failure event say
        // why the server couldn't be reached (an untrusted certificate, a DNS
        // failure, a refused connection) instead of only that it couldn't.
        Err(
            e @ (ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::Io(_)
            | ureq::Error::Timeout(_)),
        ) => return Err(anyhow::Error::new(e).context(NetworkError::Transport)),
        Err(e) => return Err(e.into()),
    };

    let headers: Vec<Header> = response
        .headers()
        .iter()
        .map(|(name, value)| Header {
            name: name.as_str().to_string(),
            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
        })
        .collect();
    handler.on_response(response.status().as_u16(), &headers)?;

    // A body that stops mid-way (the peer disconnected, the connection timed
    // out) is a network failure; only the handler's own errors (a failed
    // write) are something else.
    let mut body = response.into_body().into_reader();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match body.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => handler.on_body(&buf[..n])?,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(anyhow::Error::new(e).context(NetworkError::Transport)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_header_ignores_case() {
        let headers = vec![Header {
            name: "Content-Length".to_string(),
            value: "12".to_string(),
        }];
        assert_eq!(find_header(&headers, "content-length"), Some("12"));
        assert_eq!(find_header(&headers, "content-range"), None);
    }

    #[test]
    fn engine_tls_failure_is_a_transport_failure_with_its_cause() {
        use crate::engine_callbacks::HttpError;
        let err = engine_error(HttpError::Tls, "https://example.com");
        assert_eq!(
            err.downcast_ref::<NetworkError>(),
            Some(&NetworkError::Transport)
        );
        assert_eq!(
            format!("{err:#}"),
            "Network request failed; the server could not be reached.: TLS handshake failed"
        );
        let err = engine_error(HttpError::Protocol, "https://example.com");
        assert_eq!(err.downcast_ref::<NetworkError>(), None);
    }
}
