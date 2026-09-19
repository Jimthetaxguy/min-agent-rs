use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use std::{collections::BTreeMap, fmt, io::Read, path::Path};
use url::{Host, Url};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub connections: BTreeMap<String, Connection>,
    pub models: BTreeMap<String, ModelProfile>,
}

/// Wire protocol spoken by a connection. Each has its own adapter in `adapters`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    OpenaiChat,
    OpenaiResponses,
    AnthropicMessages,
}

impl Protocol {
    fn path_suffix(self) -> &'static str {
        match self {
            Self::OpenaiChat => "chat/completions",
            Self::OpenaiResponses => "responses",
            Self::AnthropicMessages => "messages",
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OpenaiChat => "openai_chat",
            Self::OpenaiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
        })
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub protocol: Protocol,
    pub base_url: String,
    pub auth: Auth,
    /// Explicit HTTP(S) proxy for this connection. Ambient proxy variables are never used.
    #[serde(default)]
    pub proxy: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Auth {
    None,
    /// `Authorization: Bearer <value of env>`.
    BearerEnv {
        env: String,
    },
    /// `<header>: <value of env>`, e.g. Anthropic's `x-api-key`.
    HeaderEnv {
        header: String,
        env: String,
    },
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub connection: String,
    pub model: String,
    #[serde(default = "yes")]
    pub native_tools: bool,
    pub max_output_tokens: Option<u32>,
    pub output_limit_parameter: Option<String>,
}

fn yes() -> bool {
    true
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).context("Cannot open explicit configuration file")?;
        ensure!(
            file.metadata()?.is_file(),
            "Configuration must be a regular file"
        );
        let mut data = String::new();
        file.take(131_073).read_to_string(&mut data)?;
        ensure!(data.len() <= 131_072, "Configuration exceeds 128 KiB");
        // Parser errors can contain source text, so do not propagate them.
        let config: Self = toml::from_str(&data)
            .map_err(|_| anyhow::anyhow!("Invalid configuration TOML or unknown fields"))?;
        ensure!(!config.models.is_empty(), "No model profiles configured");
        for name in config.models.keys() {
            config.resolve(name)?;
        }
        Ok(config)
    }

    pub fn resolve(&self, name: &str) -> Result<(&Connection, &ModelProfile)> {
        let profile = self.models.get(name).context("Unknown model profile")?;
        let connection = self
            .connections
            .get(&profile.connection)
            .context("Unknown connection")?;
        connection.validate()?;
        ensure!(
            !profile.model.trim().is_empty(),
            "Model ID must not be empty"
        );
        if let Some(limit) = profile.max_output_tokens {
            ensure!(limit > 0, "Output token limit must be positive");
        }
        match connection.protocol {
            Protocol::OpenaiChat => {
                if let Some(param) = &profile.output_limit_parameter {
                    ensure!(
                        matches!(param.as_str(), "max_tokens" | "max_completion_tokens"),
                        "Unsupported output token parameter"
                    );
                }
            }
            Protocol::OpenaiResponses | Protocol::AnthropicMessages => ensure!(
                profile.output_limit_parameter.is_none(),
                "output_limit_parameter applies to openai_chat only"
            ),
        }
        if connection.protocol == Protocol::AnthropicMessages {
            // Messages requires max_tokens; there is no hidden default.
            ensure!(
                profile.max_output_tokens.is_some(),
                "anthropic_messages profiles must set max_output_tokens"
            );
        }
        Ok((connection, profile))
    }
}

impl Connection {
    pub fn validate(&self) -> Result<()> {
        endpoint(&self.base_url, self.protocol)?;
        if let Some(proxy) = &self.proxy {
            proxy_url(proxy)?;
            // Plain HTTP is only allowed to numeric loopback. Through a proxy that request
            // would travel in cleartext to another host, which resolves loopback to itself.
            ensure!(
                self.endpoint()?.scheme() == "https",
                "A proxy requires an HTTPS endpoint"
            );
        }
        match &self.auth {
            Auth::None => {}
            Auth::BearerEnv { env } => env_name(env)?,
            Auth::HeaderEnv { header, env } => {
                env_name(env)?;
                ensure!(
                    !header.is_empty()
                        && header.len() <= 64
                        && header
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                    "Invalid credential header name"
                );
            }
        }
        Ok(())
    }

    pub fn endpoint(&self) -> Result<Url> {
        endpoint(&self.base_url, self.protocol)
    }

    /// Stable, secret-free identity of where requests go and how they authenticate.
    /// Recorded in traces; opaque continuation is only valid under one fingerprint.
    pub fn fingerprint(&self) -> String {
        let endpoint = self.endpoint().map(|u| u.to_string()).unwrap_or_default();
        let identity = format!(
            "{}|{}|{}|proxy={}",
            self.protocol,
            endpoint,
            self.auth.description(),
            self.proxy.is_some()
        );
        format!("{:016x}", fnv1a(identity.as_bytes()))
    }
}

/// FNV-1a 64: stable across Rust releases, unlike `DefaultHasher`. Not cryptographic.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn env_name(env: &str) -> Result<()> {
    ensure!(
        !env.is_empty() && env.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "Invalid credential environment-variable name"
    );
    Ok(())
}

fn base(raw: &str, what: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("Invalid {what} URL"))?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are forbidden"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "URL query and fragment are forbidden"
    );
    ensure!(url.host().is_some(), "{what} URL needs a host");
    // Numeric loopback only: no DNS lookup or rebinding ambiguity for HTTP.
    let loopback = match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        bail!("{what} URL requires HTTPS, or numeric loopback HTTP");
    }
    Ok(url)
}

pub fn endpoint(base_url: &str, protocol: Protocol) -> Result<Url> {
    let mut url = base(base_url, "model base")?;
    let path = format!(
        "{}/{}",
        url.path().trim_end_matches('/'),
        protocol.path_suffix()
    );
    url.set_path(&path);
    Ok(url)
}

/// Proxies may be plain HTTP (the tunnel still carries TLS to the endpoint) but must not
/// embed credentials, which would otherwise live in the config file.
pub fn proxy_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("Invalid proxy URL"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Proxy URL must be http or https"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Proxy URL credentials are forbidden"
    );
    ensure!(url.host().is_some(), "Proxy URL needs a host");
    Ok(url)
}

impl Auth {
    /// Returns the header name and secret value, if this auth kind uses one.
    pub fn credential(&self) -> Result<Option<(String, String)>> {
        let (header, env) = match self {
            Self::None => return Ok(None),
            Self::BearerEnv { env } => ("authorization".to_string(), env),
            Self::HeaderEnv { header, env } => (header.to_ascii_lowercase(), env),
        };
        let value = std::env::var(env).map_err(|_| {
            anyhow::anyhow!("Configured credential environment variable is missing")
        })?;
        ensure!(!value.trim().is_empty(), "Configured credential is empty");
        let value = if matches!(self, Self::BearerEnv { .. }) {
            format!("Bearer {value}")
        } else {
            value
        };
        Ok(Some((header, value)))
    }

    pub fn description(&self) -> String {
        match self {
            Self::None => "none".into(),
            Self::BearerEnv { env } => format!("bearer via environment variable {env}"),
            Self::HeaderEnv { header, env } => {
                format!("header {header} via environment variable {env}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_policy_and_prefixes() {
        assert_eq!(
            endpoint("https://openrouter.ai/api/v1/", Protocol::OpenaiChat)
                .unwrap()
                .as_str(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.openai.com/v1", Protocol::OpenaiResponses)
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            endpoint("https://api.anthropic.com/v1", Protocol::AnthropicMessages)
                .unwrap()
                .as_str(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:4000", Protocol::OpenaiChat)
                .unwrap()
                .path(),
            "/chat/completions"
        );
        assert!(endpoint("http://[::1]:4000/v1", Protocol::OpenaiChat).is_ok());
        for bad in [
            "http://example.com",
            "http://localhost:4000",
            "https://u:p@example.com",
            "https://example.com?key=secret",
            "file:///tmp/x",
        ] {
            assert!(endpoint(bad, Protocol::OpenaiChat).is_err(), "{bad}");
        }
    }

    #[test]
    fn proxy_policy() {
        assert!(proxy_url("http://proxy.corp:3128").is_ok());
        assert!(proxy_url("https://proxy.corp").is_ok());
        assert!(proxy_url("http://user:pass@proxy.corp:3128").is_err());
        assert!(proxy_url("socks5://proxy.corp:1080").is_err());
    }

    #[test]
    fn proxy_is_refused_for_loopback_http() {
        let mut connection = Connection {
            protocol: Protocol::OpenaiChat,
            base_url: "http://127.0.0.1:4000/v1".into(),
            auth: Auth::None,
            proxy: Some("http://proxy.corp:3128".into()),
        };
        assert!(connection.validate().is_err());
        connection.base_url = "https://gateway.example.com/v1".into();
        assert!(connection.validate().is_ok());
    }

    #[test]
    fn credentials_do_not_fallback() {
        let auth = Auth::BearerEnv {
            env: "MIN_AGENT_TEST_UNSET_938124".into(),
        };
        assert!(auth.credential().is_err());
        assert!(Auth::None.credential().unwrap().is_none());
    }

    #[test]
    fn anthropic_profiles_need_explicit_output_limit() {
        let text = r#"
            [connections.a]
            protocol = "anthropic_messages"
            base_url = "https://api.anthropic.com/v1"
            auth = { kind = "header_env", header = "x-api-key", env = "ANTHROPIC_API_KEY" }
            [models.a]
            connection = "a"
            model = "claude-x"
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert!(config.resolve("a").is_err());
        let config: Config = toml::from_str(&format!("{text}max_output_tokens = 1024\n")).unwrap();
        assert!(config.resolve("a").is_ok());
    }

    #[test]
    fn fingerprint_is_stable_and_secret_free() {
        let connection = Connection {
            protocol: Protocol::OpenaiChat,
            base_url: "https://example.com/v1".into(),
            auth: Auth::BearerEnv { env: "KEY".into() },
            proxy: None,
        };
        // Pinned: a change here silently invalidates recorded traces.
        assert_eq!(connection.fingerprint(), connection.fingerprint());
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
