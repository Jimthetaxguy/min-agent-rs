use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read, path::Path};
use url::{Host, Url};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub connections: BTreeMap<String, Connection>,
    pub models: BTreeMap<String, ModelProfile>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub protocol: String,
    pub base_url: String,
    pub auth: Auth,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Auth {
    None,
    BearerEnv { env: String },
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
        ensure!(
            connection.protocol == "openai_chat",
            "Unsupported protocol; this release implements openai_chat only"
        );
        endpoint(&connection.base_url)?;
        ensure!(
            !profile.model.trim().is_empty(),
            "Model ID must not be empty"
        );
        if let Some(limit) = profile.max_output_tokens {
            ensure!(limit > 0, "Output token limit must be positive");
        }
        if let Some(param) = &profile.output_limit_parameter {
            ensure!(
                matches!(param.as_str(), "max_tokens" | "max_completion_tokens"),
                "Unsupported output token parameter"
            );
        }
        if let Auth::BearerEnv { env } = &connection.auth {
            ensure!(
                !env.is_empty() && env.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "Invalid credential environment-variable name"
            );
        }
        Ok((connection, profile))
    }
}

pub fn endpoint(base: &str) -> Result<Url> {
    let mut url = Url::parse(base).map_err(|_| anyhow::anyhow!("Invalid model base URL"))?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are forbidden"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "URL query and fragment are forbidden"
    );
    ensure!(url.host().is_some(), "Endpoint needs a host");
    // Numeric loopback only: no DNS lookup or rebinding ambiguity for HTTP.
    let loopback = match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        bail!("Endpoint requires HTTPS, or numeric loopback HTTP");
    }
    let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

impl Auth {
    pub fn credential(&self) -> Result<Option<String>> {
        match self {
            Self::None => Ok(None),
            Self::BearerEnv { env } => {
                let value = std::env::var(env).map_err(|_| {
                    anyhow::anyhow!("Configured credential environment variable is missing")
                })?;
                ensure!(!value.trim().is_empty(), "Configured credential is empty");
                Ok(Some(value))
            }
        }
    }

    pub fn description(&self) -> String {
        match self {
            Self::None => "none".into(),
            Self::BearerEnv { env } => format!("bearer via environment variable {env}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_policy_and_prefixes() {
        assert_eq!(
            endpoint("https://openrouter.ai/api/v1/").unwrap().as_str(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:4000").unwrap().path(),
            "/chat/completions"
        );
        assert!(endpoint("http://[::1]:4000/v1").is_ok());
        for bad in [
            "http://example.com",
            "http://localhost:4000",
            "https://u:p@example.com",
            "https://example.com?key=secret",
            "file:///tmp/x",
        ] {
            assert!(endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn credentials_do_not_fallback() {
        let auth = Auth::BearerEnv {
            env: "MIN_AGENT_TEST_UNSET_938124".into(),
        };
        assert!(auth.credential().is_err());
        assert!(Auth::None.credential().unwrap().is_none());
    }
}
