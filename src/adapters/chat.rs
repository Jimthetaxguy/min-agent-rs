//! OpenAI Chat Completions (`/chat/completions`), also served by OpenRouter, LiteLLM,
//! and most OpenAI-compatible local servers.
use super::{finish, non_empty, string_arguments, tokens};
use crate::{
    config::ModelProfile,
    model::{invalid, Item, ModelError, ModelTurn, ProviderFailure, ToolCall, ToolSpec, Usage},
};
use serde_json::{json, Value};

pub fn request(profile: &ModelProfile, system: &str, items: &[Item], tools: &[ToolSpec]) -> Value {
    let mut messages = vec![json!({"role":"system","content":system})];
    for item in items {
        messages.push(match item {
            Item::User(text) => json!({"role":"user","content":text}),
            Item::Assistant(native) => native.clone(),
            Item::ToolResult {
                call_id, content, ..
            } => json!({"role":"tool","tool_call_id":call_id,"content":content}),
        });
    }
    let mut body = json!({"model": profile.model, "messages": messages, "stream": false});
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|t| json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.parameters}}))
            .collect();
    }
    if let Some(max) = profile.max_output_tokens {
        body[profile
            .output_limit_parameter
            .as_deref()
            .unwrap_or("max_tokens")] = json!(max);
    }
    body
}

pub fn parse(response: Value) -> Result<ModelTurn, ModelError> {
    if response.get("error").is_some_and(|e| !e.is_null()) {
        return Err(ProviderFailure::ErrorEnvelope.into());
    }
    let Some(choices) = response["choices"].as_array() else {
        return invalid("missing choices");
    };
    if choices.len() != 1 {
        return invalid("expected exactly one choice");
    }
    let choice = &choices[0];
    if choice.get("error").is_some_and(|e| !e.is_null()) {
        return Err(ProviderFailure::ErrorEnvelope.into());
    }
    let wants_tools = match choice["finish_reason"].as_str() {
        Some("stop") => false,
        Some("tool_calls") => true,
        Some("length") => return invalid("incomplete response (length)"),
        Some("content_filter") => return invalid("model refused (content filter)"),
        Some(_) => return invalid("unsupported finish reason"),
        None => return invalid("missing completion reason"),
    };
    let message = &choice["message"];
    if message["role"] != "assistant" {
        return invalid("expected assistant role");
    }
    if !message.get("refusal").is_none_or(Value::is_null) {
        return invalid("model refused");
    }
    if message.get("function_call").is_some_and(|f| !f.is_null()) {
        return invalid("legacy function calls unsupported");
    }
    let text = match message.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        _ => return invalid("non-text content unsupported"),
    };
    let raw_calls: &[Value] = match message.get("tool_calls") {
        None | Some(Value::Null) => &[],
        Some(Value::Array(calls)) => calls,
        _ => return invalid("invalid tool_calls field"),
    };
    let mut calls = Vec::new();
    for raw in raw_calls {
        if raw["type"] != "function" {
            return invalid("unsupported tool type");
        }
        calls.push(ToolCall {
            id: non_empty(&raw["id"], "missing tool call ID")?.into(),
            name: non_empty(&raw["function"]["name"], "missing tool name")?.into(),
            arguments: string_arguments(raw["function"].get("arguments"))?,
        });
    }
    let usage = response
        .get("usage")
        .filter(|u| u.is_object())
        .map(|u| Usage {
            input_tokens: tokens(&u["prompt_tokens"]),
            output_tokens: tokens(&u["completion_tokens"]),
            reasoning_tokens: tokens(&u["completion_tokens_details"]["reasoning_tokens"]),
        });
    // Preserve extensions (including opaque reasoning) verbatim within this run.
    finish(message.clone(), text, calls, wants_tools, usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::conformance::{check, profile, sample_conversation, Expect::*};
    use crate::config::Protocol;

    fn call(id: &str, args: &str) -> Value {
        json!({"id":id,"type":"function","function":{"name":"read_file","arguments":args}})
    }

    fn reply(reason: &str, message: Value) -> Value {
        json!({"choices":[{"finish_reason":reason,"message":message}]})
    }

    fn calls(reason: &str, calls: Vec<Value>) -> Value {
        reply(
            reason,
            json!({"role":"assistant","content":null,"tool_calls":calls}),
        )
    }

    #[test]
    fn conformance() {
        check(
            Protocol::OpenaiChat,
            vec![
                (
                    "single_call",
                    calls("tool_calls", vec![call("a", "{}")]),
                    Calls(1),
                ),
                (
                    "multiple_calls",
                    calls("tool_calls", vec![call("a", "{}"), call("b", "{}")]),
                    Calls(2),
                ),
                (
                    "final_text",
                    reply("stop", json!({"role":"assistant","content":"done"})),
                    Final,
                ),
                (
                    "opaque_reasoning_preserved",
                    reply(
                        "stop",
                        json!({"role":"assistant","content":"x","reasoning_content":"opaque"}),
                    ),
                    Final,
                ),
                (
                    "refusal",
                    reply(
                        "stop",
                        json!({"role":"assistant","content":null,"refusal":"no"}),
                    ),
                    Invalid,
                ),
                ("truncated", calls("length", vec![call("a", "{}")]), Invalid),
                (
                    "empty_final",
                    reply("stop", json!({"role":"assistant","content":" "})),
                    Invalid,
                ),
                (
                    "missing_call_id",
                    calls("tool_calls", vec![call("", "{}")]),
                    Invalid,
                ),
                (
                    "malformed_arguments",
                    calls("tool_calls", vec![call("a", "{")]),
                    Invalid,
                ),
                (
                    "array_arguments",
                    calls("tool_calls", vec![call("a", "[]")]),
                    Invalid,
                ),
                (
                    "duplicate_ids",
                    calls("tool_calls", vec![call("a", "{}"), call("a", "{}")]),
                    Invalid,
                ),
                (
                    "error_envelope",
                    json!({"error":{"message":"bad","type":"server_error"}}),
                    Provider,
                ),
                (
                    "reason_mismatch",
                    calls("stop", vec![call("a", "{}")]),
                    Invalid,
                ),
                (
                    "unknown_item",
                    calls(
                        "tool_calls",
                        vec![json!({"id":"a","type":"custom","custom":{}})],
                    ),
                    Invalid,
                ),
                (
                    "usage_reported",
                    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"x"}}],"usage":{"prompt_tokens":3,"completion_tokens":4}}),
                    Final,
                ),
            ],
        );
    }

    #[test]
    fn preserves_native_message_and_usage() {
        let turn = parse(json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"x","reasoning_content":"opaque"}}],"usage":{"prompt_tokens":3,"completion_tokens":4}})).unwrap();
        assert_eq!(turn.native["reasoning_content"], "opaque");
        let usage = turn.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(3));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.reasoning_tokens, None);
    }

    #[test]
    fn request_shape() {
        let mut items = sample_conversation();
        items.push(Item::Assistant(json!({"role":"assistant","tool_calls":[]})));
        items.push(Item::ToolResult {
            call_id: "a".into(),
            content: "{}".into(),
            is_error: false,
        });
        let body = request(
            &profile(Some(9)),
            "sys",
            &items,
            &crate::tools::definitions(),
        );
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "question");
        assert_eq!(body["messages"][3]["tool_call_id"], "a");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["max_tokens"], 9);
        assert_eq!(body["stream"], false);
        let body = request(&profile(None), "sys", &items, &[]);
        assert!(body.get("tools").is_none());
    }
}
