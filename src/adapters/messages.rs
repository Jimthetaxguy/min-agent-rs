//! Anthropic Messages (`/messages`). Assistant content blocks (including thinking
//! signatures) are replayed verbatim; tool results for one assistant turn are grouped into
//! the single user message that must immediately follow it.
use super::{finish, non_empty, object_arguments, tokens};
use crate::{
    config::ModelProfile,
    model::{invalid, Item, ModelError, ModelTurn, ProviderFailure, ToolCall, ToolSpec, Usage},
};
use serde_json::{json, Value};

pub fn request(profile: &ModelProfile, system: &str, items: &[Item], tools: &[ToolSpec]) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    let mut results: Vec<Value> = Vec::new();
    let flush = |messages: &mut Vec<Value>, results: &mut Vec<Value>| {
        if !results.is_empty() {
            messages.push(json!({"role":"user","content":std::mem::take(results)}));
        }
    };
    for item in items {
        match item {
            Item::ToolResult {
                call_id,
                content,
                is_error,
            } => results.push(json!({"type":"tool_result","tool_use_id":call_id,"content":content,"is_error":is_error})),
            Item::User(text) => {
                flush(&mut messages, &mut results);
                messages.push(json!({"role":"user","content":text}));
            }
            Item::Assistant(content) => {
                flush(&mut messages, &mut results);
                messages.push(json!({"role":"assistant","content":content}));
            }
        }
    }
    flush(&mut messages, &mut results);
    let mut body = json!({
        "model": profile.model,
        "system": system,
        "messages": messages,
        "stream": false,
    });
    // Config resolution requires this for anthropic_messages; there is no hidden default.
    if let Some(max) = profile.max_output_tokens {
        body["max_tokens"] = json!(max);
    }
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.parameters}))
            .collect();
    }
    body
}

pub fn parse(response: Value) -> Result<ModelTurn, ModelError> {
    if response["type"] == "error" || response.get("error").is_some_and(|e| !e.is_null()) {
        return Err(ProviderFailure::ErrorEnvelope.into());
    }
    if response["role"] != "assistant" {
        return invalid("expected assistant role");
    }
    let wants_tools = match response["stop_reason"].as_str() {
        Some("end_turn" | "stop_sequence") => false,
        Some("tool_use") => true,
        Some("max_tokens") => return invalid("incomplete response (max_tokens)"),
        Some("refusal") => return invalid("model refused"),
        Some(_) => return invalid("unsupported stop reason"),
        None => return invalid("missing stop reason"),
    };
    let Some(content) = response["content"].as_array() else {
        return invalid("content must be an array");
    };
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in content {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("thinking" | "redacted_thinking") => {}
            Some("tool_use") => calls.push(ToolCall {
                id: non_empty(&block["id"], "missing tool call ID")?.into(),
                name: non_empty(&block["name"], "missing tool name")?.into(),
                arguments: object_arguments(block["input"].clone())?,
            }),
            _ => return invalid("unsupported content block"),
        }
    }
    let usage = response
        .get("usage")
        .filter(|u| u.is_object())
        .map(|u| Usage {
            input_tokens: tokens(&u["input_tokens"]),
            output_tokens: tokens(&u["output_tokens"]),
            reasoning_tokens: None,
        });
    finish(
        Value::Array(content.clone()),
        text,
        calls,
        wants_tools,
        usage,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::conformance::{check, profile, sample_conversation, Expect::*};
    use crate::config::Protocol;

    fn tool_use(id: &str, input: Value) -> Value {
        json!({"type":"tool_use","id":id,"name":"read_file","input":input})
    }

    fn reply(stop: &str, content: Vec<Value>) -> Value {
        json!({"type":"message","role":"assistant","stop_reason":stop,"content":content})
    }

    #[test]
    fn conformance() {
        let text = |t: &str| json!({"type":"text","text":t});
        check(
            Protocol::AnthropicMessages,
            vec![
                (
                    "single_call",
                    reply("tool_use", vec![tool_use("a", json!({}))]),
                    Calls(1),
                ),
                (
                    "multiple_calls",
                    reply(
                        "tool_use",
                        vec![
                            text("checking"),
                            tool_use("a", json!({})),
                            tool_use("b", json!({})),
                        ],
                    ),
                    Calls(2),
                ),
                ("final_text", reply("end_turn", vec![text("done")]), Final),
                (
                    "opaque_reasoning_preserved",
                    reply(
                        "tool_use",
                        vec![
                            json!({"type":"thinking","thinking":"t","signature":"sig"}),
                            tool_use("a", json!({})),
                        ],
                    ),
                    Calls(1),
                ),
                ("refusal", reply("refusal", vec![]), Invalid),
                (
                    "truncated",
                    reply("max_tokens", vec![tool_use("a", json!({}))]),
                    Invalid,
                ),
                ("empty_final", reply("end_turn", vec![]), Invalid),
                (
                    "missing_call_id",
                    reply("tool_use", vec![tool_use("", json!({}))]),
                    Invalid,
                ),
                (
                    "malformed_arguments",
                    reply("tool_use", vec![tool_use("a", json!("{"))]),
                    Invalid,
                ),
                (
                    "array_arguments",
                    reply("tool_use", vec![tool_use("a", json!([]))]),
                    Invalid,
                ),
                (
                    "duplicate_ids",
                    reply(
                        "tool_use",
                        vec![tool_use("a", json!({})), tool_use("a", json!({}))],
                    ),
                    Invalid,
                ),
                (
                    "error_envelope",
                    json!({"type":"error","error":{"type":"overloaded_error","message":"x"}}),
                    Provider,
                ),
                (
                    "reason_mismatch",
                    reply("end_turn", vec![tool_use("a", json!({}))]),
                    Invalid,
                ),
                (
                    "unknown_item",
                    reply("end_turn", vec![json!({"type":"server_tool_use","id":"s"})]),
                    Invalid,
                ),
                (
                    "usage_reported",
                    json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[text("x")],"usage":{"input_tokens":1,"output_tokens":2}}),
                    Final,
                ),
            ],
        );
    }

    #[test]
    fn groups_tool_results_after_assistant_turn() {
        let turn = parse(reply(
            "tool_use",
            vec![
                json!({"type":"thinking","thinking":"t","signature":"sig"}),
                tool_use("a", json!({"path":"x"})),
                tool_use("b", json!({})),
            ],
        ))
        .unwrap();
        let mut items = sample_conversation();
        items.push(Item::Assistant(turn.native));
        for id in ["a", "b"] {
            items.push(Item::ToolResult {
                call_id: id.into(),
                content: "{}".into(),
                is_error: id == "b",
            });
        }
        let body = request(
            &profile(Some(64)),
            "sys",
            &items,
            &crate::tools::definitions(),
        );
        assert_eq!(body["system"], "sys");
        assert_eq!(body["max_tokens"], 64);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"][0]["signature"], "sig");
        let results = messages[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "a");
        assert_eq!(results[1]["is_error"], true);
        assert!(body["tools"][0]["input_schema"].is_object());
    }
}
