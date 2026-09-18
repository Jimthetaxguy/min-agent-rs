use crate::config::{endpoint, Connection, ModelProfile};
use anyhow::{bail, ensure, Context, Result};
use reqwest::{blocking::Client, header};
use serde_json::{json, Value};
use std::{collections::HashSet, io::Read, time::Duration};

pub const RESPONSE_LIMIT: usize = 1_048_576;
pub const CONTEXT_LIMIT: usize = 262_144;

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug)]
pub struct ModelTurn {
    pub message: Value,
    pub text: String,
    pub calls: Vec<ToolCall>,
}

pub trait ModelClient {
    fn turn(&self, messages: &[Value], tools: &[Value], remaining: Duration) -> Result<ModelTurn>;
}

pub struct ChatClient {
    client: Client,
    endpoint: url::Url,
    profile: ModelProfile,
}

impl ChatClient {
    pub fn new(connection: &Connection, profile: &ModelProfile) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        if let Some(key) = connection.auth.credential()? {
            let mut value = header::HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| anyhow::anyhow!("Credential is not a valid HTTP header"))?;
            value.set_sensitive(true);
            headers.insert(header::AUTHORIZATION, value);
        }
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .no_proxy()
            .build()
            .context("Cannot construct HTTP client")?;
        Ok(Self {
            client,
            endpoint: endpoint(&connection.base_url)?,
            profile: profile.clone(),
        })
    }
}

impl ModelClient for ChatClient {
    fn turn(&self, messages: &[Value], tools: &[Value], remaining: Duration) -> Result<ModelTurn> {
        ensure!(!remaining.is_zero(), "BudgetExceeded: wall-clock deadline");
        let mut body = json!({"model": self.profile.model, "messages": messages, "stream": false});
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        if let Some(max) = self.profile.max_output_tokens {
            body[self
                .profile
                .output_limit_parameter
                .as_deref()
                .unwrap_or("max_tokens")] = json!(max);
        }
        let bytes = serde_json::to_vec(&body)?;
        ensure!(
            bytes.len() <= CONTEXT_LIMIT,
            "BudgetExceeded: request/context bytes"
        );
        let response = self
            .client
            .post(self.endpoint.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .timeout(remaining.min(Duration::from_secs(90)))
            .body(bytes)
            .send()
            .map_err(|_| anyhow::anyhow!("ProviderError: HTTP transport failed or timed out"))?;
        ensure!(
            response.status().is_success(),
            "ProviderError: HTTP status {}",
            response.status().as_u16()
        );
        if let Some(len) = response.content_length() {
            ensure!(
                len <= RESPONSE_LIMIT as u64,
                "ProviderError: response exceeds byte limit"
            );
        }
        let mut bytes = Vec::new();
        response
            .take(RESPONSE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("ProviderError: response read failed"))?;
        ensure!(
            bytes.len() <= RESPONSE_LIMIT,
            "ProviderError: response exceeds byte limit"
        );
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("InvalidResponse: malformed JSON"))?;
        parse_turn(response)
    }
}

pub fn parse_turn(response: Value) -> Result<ModelTurn> {
    ensure!(
        response.get("error").is_none(),
        "ProviderError: API error envelope"
    );
    let choices = response["choices"]
        .as_array()
        .context("InvalidResponse: missing choices")?;
    ensure!(
        choices.len() == 1,
        "InvalidResponse: expected exactly one choice"
    );
    let choice = &choices[0];
    ensure!(choice.get("error").is_none(), "ProviderError: choice error");
    let reason = choice["finish_reason"]
        .as_str()
        .context("InvalidResponse: missing completion reason")?;
    ensure!(
        matches!(reason, "stop" | "tool_calls"),
        "InvalidResponse: incomplete, refused, or unsupported finish reason"
    );
    let message = &choice["message"];
    ensure!(
        message["role"] == "assistant",
        "InvalidResponse: expected assistant role"
    );
    ensure!(
        message.get("refusal").is_none_or(Value::is_null),
        "InvalidResponse: model refused"
    );
    ensure!(
        message.get("function_call").is_none(),
        "InvalidResponse: legacy function calls unsupported"
    );
    let text = match message.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        _ => bail!("InvalidResponse: non-text content unsupported"),
    };
    let raw_calls: &[Value] = match message.get("tool_calls") {
        None | Some(Value::Null) => &[],
        Some(Value::Array(calls)) => calls,
        _ => bail!("InvalidResponse: invalid tool_calls field"),
    };
    ensure!(
        raw_calls.len() <= 40,
        "InvalidResponse: excessive tool batch"
    );
    let mut ids = HashSet::new();
    let mut calls = Vec::new();
    for raw in raw_calls {
        ensure!(
            raw["type"] == "function",
            "InvalidResponse: unsupported tool type"
        );
        let id = raw["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("InvalidResponse: missing tool call ID")?;
        ensure!(ids.insert(id), "InvalidResponse: duplicate tool call ID");
        let name = raw["function"]["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("InvalidResponse: missing tool name")?;
        let args = raw["function"]["arguments"]
            .as_str()
            .context("InvalidResponse: missing arguments string")?;
        let arguments: Value = serde_json::from_str(args)
            .map_err(|_| anyhow::anyhow!("InvalidResponse: malformed tool arguments"))?;
        ensure!(
            arguments.is_object(),
            "InvalidResponse: tool arguments must be an object"
        );
        calls.push(ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        });
    }
    ensure!(
        (reason == "tool_calls") != calls.is_empty(),
        "InvalidResponse: finish reason and tool calls disagree"
    );
    ensure!(
        !calls.is_empty() || !text.trim().is_empty(),
        "InvalidResponse: empty final answer"
    );
    // Preserve extensions (including opaque reasoning) verbatim within this run.
    Ok(ModelTurn {
        message: message.clone(),
        text,
        calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(args: &str, reason: &str) -> Value {
        json!({"choices":[{"finish_reason":reason,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"c","type":"function","function":{"name":"read_file","arguments":args}}]}}]})
    }

    #[test]
    fn validates_native_arguments_and_completion() {
        assert!(parse_turn(response("{}", "tool_calls")).is_ok());
        for (args, reason) in [
            ("{", "tool_calls"),
            ("[]", "tool_calls"),
            ("{}", "length"),
            ("{}", "stop"),
        ] {
            assert!(parse_turn(response(args, reason)).is_err());
        }
    }

    #[test]
    fn rejects_duplicate_ids() {
        let mut r = response("{}", "tool_calls");
        let c = r["choices"][0]["message"]["tool_calls"][0].clone();
        r["choices"][0]["message"]["tool_calls"]
            .as_array_mut()
            .unwrap()
            .push(c);
        assert!(parse_turn(r).is_err());
    }
}
