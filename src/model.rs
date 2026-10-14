//! Protocol-neutral model seam plus the one bounded HTTP transport all adapters share.
use crate::{
    adapters,
    config::{proxy_url, Connection, ModelProfile, Protocol},
};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::{fmt, time::Duration};
use ureq::http::{header, HeaderMap, HeaderName, HeaderValue};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Tool schema in neutral form; each adapter renders its own wire shape.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// One conversation entry. `Assistant` holds the provider's native output verbatim
/// (Chat message, Responses output items, or Messages content blocks) so opaque
/// continuation such as reasoning replays unchanged within the same run and client.
#[derive(Debug, Clone)]
pub enum Item {
    User(String),
    Assistant(Value),
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
}

/// Token counts as reported by the provider. Unknown is `None`, never zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug)]
pub struct ModelTurn {
    pub native: Value,
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

/// Per-request limits handed down from the run's `Budget`.
#[derive(Debug, Clone, Copy)]
pub struct RequestLimits {
    pub timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProviderFailure {
    Timeout,
    Connect,
    Transport,
    Status {
        code: u16,
        #[serde(skip)]
        retry_after: Option<Duration>,
    },
    BodyTooLarge,
    BodyRead,
    UnexpectedContentType,
    ErrorEnvelope,
}

impl ProviderFailure {
    /// Model requests are effect-free for this agent, so transient failures may be
    /// retried within the run deadline. Client errors (4xx other than 408/429) are not.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Timeout | Self::Connect | Self::BodyRead => true,
            Self::Status { code, .. } => matches!(code, 408 | 429 | 500 | 502 | 503 | 504 | 529),
            _ => false,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("request timed out"),
            Self::Connect => f.write_str("connection failed"),
            Self::Transport => f.write_str("HTTP transport failed"),
            Self::Status { code, .. } => write!(f, "HTTP status {code}"),
            Self::BodyTooLarge => f.write_str("response exceeds byte limit"),
            Self::BodyRead => f.write_str("response read failed"),
            Self::UnexpectedContentType => f.write_str("unexpected response content type"),
            Self::ErrorEnvelope => f.write_str("API error envelope"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelError {
    /// The serialized request would exceed the context byte budget; nothing was sent.
    RequestTooLarge,
    Provider(ProviderFailure),
    /// The response violated the protocol contract; no call in it is authorized.
    Invalid(String),
}

impl From<ProviderFailure> for ModelError {
    fn from(failure: ProviderFailure) -> Self {
        Self::Provider(failure)
    }
}

pub(crate) fn invalid<T>(reason: &str) -> Result<T, ModelError> {
    Err(ModelError::Invalid(reason.to_string()))
}

pub trait ModelClient {
    fn turn(
        &self,
        system: &str,
        items: &[Item],
        tools: &[ToolSpec],
        limits: RequestLimits,
    ) -> Result<ModelTurn, ModelError>;
}

/// HTTP model client for any configured protocol.
pub struct HttpModelClient {
    agent: ureq::Agent,
    headers: HeaderMap,
    endpoint: url::Url,
    protocol: Protocol,
    profile: ModelProfile,
}

impl HttpModelClient {
    pub fn new(connection: &Connection, profile: &ModelProfile) -> Result<Self> {
        connection.validate()?;
        let mut headers = HeaderMap::new();
        if let Some((name, secret)) = connection.auth.credential()? {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| anyhow::anyhow!("Invalid credential header name"))?;
            let mut value = HeaderValue::from_str(&secret)
                .map_err(|_| anyhow::anyhow!("Credential is not a valid HTTP header"))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        if connection.protocol == Protocol::AnthropicMessages {
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        }
        // ureq's default config reads ALL_PROXY / HTTP(S)_PROXY. Replace that with none,
        // then install only the explicit proxy from this connection.
        let mut builder = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(10)))
            .proxy(None);
        if let Some(proxy) = &connection.proxy {
            let proxy = ureq::Proxy::new(proxy_url(proxy)?.as_str())
                .context("Invalid proxy configuration")?;
            builder = builder.proxy(Some(proxy));
        }
        Ok(Self {
            agent: builder.build().into(),
            headers,
            endpoint: connection.endpoint()?,
            protocol: connection.protocol,
            profile: profile.clone(),
        })
    }

    fn post(&self, body: &Value, limits: RequestLimits) -> Result<Value, ModelError> {
        let bytes = serde_json::to_vec(body).map_err(|_| ModelError::RequestTooLarge)?;
        if bytes.len() > limits.max_request_bytes {
            return Err(ModelError::RequestTooLarge);
        }
        let mut request = self
            .agent
            .post(self.endpoint.as_str())
            .config()
            .timeout_global(Some(limits.timeout))
            .build();
        if let Some(dest) = request.headers_mut() {
            for (name, value) in &self.headers {
                dest.insert(name.clone(), value.clone());
            }
        }
        let response = request
            .content_type("application/json")
            .send(&bytes)
            .map_err(classify_send)?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(ProviderFailure::Status {
                code: status.as_u16(),
                retry_after,
            }
            .into());
        }
        // Absent Content-Type is tolerated (some local servers omit it); anything that is
        // not JSON, notably an SSE stream answering a `stream:false` request, is rejected.
        if let Some(kind) = response.headers().get(header::CONTENT_TYPE) {
            let kind = kind.to_str().unwrap_or("").to_ascii_lowercase();
            if !kind.starts_with("application/json") {
                return Err(ProviderFailure::UnexpectedContentType.into());
            }
        }
        if response
            .body()
            .content_length()
            .is_some_and(|len| len > limits.max_response_bytes as u64)
        {
            return Err(ProviderFailure::BodyTooLarge.into());
        }
        // Enforced while reading: a chunked body without Content-Length cannot exceed it.
        let cap = limits.max_response_bytes as u64 + 1;
        let bytes = response
            .into_body()
            .into_with_config()
            .limit(cap)
            .read_to_vec()
            .map_err(classify_body)?;
        if bytes.len() > limits.max_response_bytes {
            return Err(ProviderFailure::BodyTooLarge.into());
        }
        serde_json::from_slice(&bytes).or_else(|_| invalid("malformed JSON"))
    }
}

fn classify_send(error: ureq::Error) -> ProviderFailure {
    if is_timeout(&error) {
        ProviderFailure::Timeout
    } else if is_connect(&error) {
        ProviderFailure::Connect
    } else {
        ProviderFailure::Transport
    }
}

fn classify_body(error: ureq::Error) -> ProviderFailure {
    if matches!(error, ureq::Error::BodyExceedsLimit(_)) {
        ProviderFailure::BodyTooLarge
    } else if is_timeout(&error) {
        ProviderFailure::Timeout
    } else {
        ProviderFailure::BodyRead
    }
}

fn is_timeout(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => io_timed_out(io),
        _ => false,
    }
}

fn io_timed_out(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::TimedOut {
        return true;
    }
    let mut source: Option<&(dyn std::error::Error + 'static)> = error.get_ref().map(|e| e as _);
    while let Some(current) = source {
        if matches!(current.downcast_ref::<ureq::Error>(), Some(inner) if is_timeout(inner))
            || current
                .downcast_ref::<std::io::Error>()
                .is_some_and(|inner| inner.kind() == std::io::ErrorKind::TimedOut)
        {
            return true;
        }
        source = current.source();
    }
    false
}

/// Connection failures that are not themselves timeouts. A connect timeout is `Timeout`,
/// matching the previous client, which checked the deadline before the connect class.
fn is_connect(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::ConnectionFailed
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectProxyFailed(_) => true,
        ureq::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::AddrNotAvailable
        ),
        _ => false,
    }
}

impl ModelClient for HttpModelClient {
    fn turn(
        &self,
        system: &str,
        items: &[Item],
        tools: &[ToolSpec],
        limits: RequestLimits,
    ) -> Result<ModelTurn, ModelError> {
        let body = adapters::request(self.protocol, &self.profile, system, items, tools);
        let response = self.post(&body, limits)?;
        adapters::parse(self.protocol, response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classification() {
        for code in [408, 429, 500, 502, 503, 504, 529] {
            assert!(ProviderFailure::Status {
                code,
                retry_after: None
            }
            .retryable());
        }
        for code in [400, 401, 403, 404, 422] {
            assert!(!ProviderFailure::Status {
                code,
                retry_after: None
            }
            .retryable());
        }
        assert!(ProviderFailure::Timeout.retryable());
        assert!(!ProviderFailure::BodyTooLarge.retryable());
        assert!(!ProviderFailure::UnexpectedContentType.retryable());
    }
}
