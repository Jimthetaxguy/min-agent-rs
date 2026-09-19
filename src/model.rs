//! Protocol-neutral model seam plus the one bounded HTTP transport all adapters share.
use crate::{
    adapters,
    config::{proxy_url, Connection, ModelProfile, Protocol},
};
use anyhow::{Context, Result};
use reqwest::{blocking::Client, header};
use serde::Serialize;
use serde_json::Value;
use std::{fmt, io::Read, time::Duration};

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
    client: Client,
    endpoint: url::Url,
    protocol: Protocol,
    profile: ModelProfile,
}

impl HttpModelClient {
    pub fn new(connection: &Connection, profile: &ModelProfile) -> Result<Self> {
        connection.validate()?;
        let mut headers = header::HeaderMap::new();
        if let Some((name, secret)) = connection.auth.credential()? {
            let name = header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| anyhow::anyhow!("Invalid credential header name"))?;
            let mut value = header::HeaderValue::from_str(&secret)
                .map_err(|_| anyhow::anyhow!("Credential is not a valid HTTP header"))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        if connection.protocol == Protocol::AnthropicMessages {
            headers.insert(
                "anthropic-version",
                header::HeaderValue::from_static("2023-06-01"),
            );
        }
        // `no_proxy` clears ambient proxy discovery; an explicit proxy is added after.
        let mut builder = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .no_proxy();
        if let Some(proxy) = &connection.proxy {
            let proxy = reqwest::Proxy::all(proxy_url(proxy)?.as_str())
                .context("Invalid proxy configuration")?;
            builder = builder.proxy(proxy);
        }
        Ok(Self {
            client: builder.build().context("Cannot construct HTTP client")?,
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
        let response = self
            .client
            .post(self.endpoint.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .timeout(limits.timeout)
            .body(bytes)
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    ProviderFailure::Timeout
                } else if error.is_connect() {
                    ProviderFailure::Connect
                } else {
                    ProviderFailure::Transport
                }
            })?;
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
            .content_length()
            .is_some_and(|len| len > limits.max_response_bytes as u64)
        {
            return Err(ProviderFailure::BodyTooLarge.into());
        }
        // Enforced while reading: a chunked body without Content-Length cannot exceed it.
        let mut bytes = Vec::new();
        response
            .take(limits.max_response_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                if read_timed_out(&error) {
                    ProviderFailure::Timeout
                } else {
                    ProviderFailure::BodyRead
                }
            })?;
        if bytes.len() > limits.max_response_bytes {
            return Err(ProviderFailure::BodyTooLarge.into());
        }
        serde_json::from_slice(&bytes).or_else(|_| invalid("malformed JSON"))
    }
}

/// Body reads surface the request deadline as an `io::Error` wrapping a reqwest timeout.
fn read_timed_out(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::TimedOut {
        return true;
    }
    let mut source: Option<&(dyn std::error::Error + 'static)> = error.get_ref().map(|e| e as _);
    while let Some(current) = source {
        if current
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
            || current
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::TimedOut)
        {
            return true;
        }
        source = current.source();
    }
    false
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
