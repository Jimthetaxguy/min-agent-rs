//! OpenAI Responses (`/responses`), used statelessly: `store:false`, with the complete
//! ordered output (including encrypted reasoning items) replayed as input on the next turn.
use super::{finish, non_empty, string_arguments, tokens};
use crate::{
    config::ModelProfile,
    model::{invalid, Item, ModelError, ModelTurn, ProviderFailure, ToolCall, ToolSpec, Usage},
};
use serde_json::{json, Value};

pub fn request(profile: &ModelProfile, system: &str, items: &[Item], tools: &[ToolSpec]) -> Value {
    let mut input = Vec::new();
    for item in items {
        match item {
            Item::User(text) => input.push(json!({"role":"user","content":text})),
            Item::Assistant(Value::Array(output)) => input.extend(output.iter().cloned()),
            Item::Assistant(other) => input.push(other.clone()),
            Item::ToolResult {
                call_id, content, ..
            } => input
                .push(json!({"type":"function_call_output","call_id":call_id,"output":content})),
        }
    }
    let mut body = json!({
        "model": profile.model,
        "instructions": system,
        "input": input,
        "store": false,
        "stream": false,
        "include": ["reasoning.encrypted_content"],
    });
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|t| json!({"type":"function","name":t.name,"description":t.description,"parameters":t.parameters,"strict":false}))
            .collect();
    }
    if let Some(max) = profile.max_output_tokens {
        body["max_output_tokens"] = json!(max);
    }
    body
}

pub fn parse(response: Value) -> Result<ModelTurn, ModelError> {
    if response.get("error").is_some_and(|e| !e.is_null()) {
        return Err(ProviderFailure::ErrorEnvelope.into());
    }
    match response["status"].as_str() {
        Some("completed") => {}
        Some("incomplete") => return invalid("incomplete response"),
        Some("failed") => return Err(ProviderFailure::ErrorEnvelope.into()),
        _ => return invalid("unsupported response status"),
    }
    let Some(output) = response["output"].as_array() else {
        return invalid("missing output");
    };
    let mut text = String::new();
    let mut calls = Vec::new();
    for item in output {
        match item["type"].as_str() {
            Some("reasoning") => {}
            Some("message") => {
                if item["role"] != "assistant" {
                    return invalid("expected assistant role");
                }
                let Some(parts) = item["content"].as_array() else {
                    return invalid("message content must be an array");
                };
                for part in parts {
                    match part["type"].as_str() {
                        Some("output_text") => {
                            text.push_str(part["text"].as_str().unwrap_or_default())
                        }
                        Some("refusal") => return invalid("model refused"),
                        _ => return invalid("unsupported message content"),
                    }
                }
            }
            Some("function_call") => calls.push(ToolCall {
                id: non_empty(&item["call_id"], "missing tool call ID")?.into(),
                name: non_empty(&item["name"], "missing tool name")?.into(),
                arguments: string_arguments(item.get("arguments"))?,
            }),
            _ => return invalid("unsupported output item"),
        }
    }
    let usage = response
        .get("usage")
        .filter(|u| u.is_object())
        .map(|u| Usage {
            input_tokens: tokens(&u["input_tokens"]),
            output_tokens: tokens(&u["output_tokens"]),
            reasoning_tokens: tokens(&u["output_tokens_details"]["reasoning_tokens"]),
        });
    // Responses has no separate stop reason: requested tools are exactly the calls present.
    let wants_tools = !calls.is_empty();
    finish(
        Value::Array(output.clone()),
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

    fn call(id: &str, args: &str) -> Value {
        json!({"type":"function_call","id":"fc_1","call_id":id,"name":"read_file","arguments":args,"status":"completed"})
    }

    fn text(t: &str) -> Value {
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":t}]})
    }

    fn done(output: Vec<Value>) -> Value {
        json!({"status":"completed","output":output})
    }

    #[test]
    fn conformance() {
        let reasoning =
            json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque"});
        check(
            Protocol::OpenaiResponses,
            vec![
                ("single_call", done(vec![call("a", "{}")]), Calls(1)),
                (
                    "multiple_calls",
                    done(vec![call("a", "{}"), call("b", "{}")]),
                    Calls(2),
                ),
                ("final_text", done(vec![text("done")]), Final),
                (
                    "opaque_reasoning_preserved",
                    done(vec![reasoning.clone(), call("a", "{}")]),
                    Calls(1),
                ),
                (
                    "refusal",
                    done(vec![
                        json!({"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}),
                    ]),
                    Invalid,
                ),
                (
                    "truncated",
                    json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[call("a","{\"pa")]}),
                    Invalid,
                ),
                ("empty_final", done(vec![text("")]), Invalid),
                ("missing_call_id", done(vec![call("", "{}")]), Invalid),
                ("malformed_arguments", done(vec![call("a", "{")]), Invalid),
                ("array_arguments", done(vec![call("a", "[]")]), Invalid),
                (
                    "duplicate_ids",
                    done(vec![call("a", "{}"), call("a", "{}")]),
                    Invalid,
                ),
                (
                    "error_envelope",
                    json!({"status":"failed","error":{"code":"server_error","message":"x"},"output":[]}),
                    Provider,
                ),
                ("reason_mismatch", done(vec![]), Invalid),
                (
                    "unknown_item",
                    done(vec![json!({"type":"web_search_call","id":"ws"})]),
                    Invalid,
                ),
                (
                    "usage_reported",
                    json!({"status":"completed","output":[text("x")],"usage":{"input_tokens":5,"output_tokens":6,"output_tokens_details":{"reasoning_tokens":2}}}),
                    Final,
                ),
            ],
        );
    }

    #[test]
    fn replays_complete_output_in_order() {
        let output = json!([
            {"type":"reasoning","id":"rs_1","encrypted_content":"opaque"},
            {"type":"function_call","call_id":"a","name":"read_file","arguments":"{}"}
        ]);
        let turn = parse(json!({"status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":6,"output_tokens_details":{"reasoning_tokens":2}}})).unwrap();
        assert_eq!(turn.usage.unwrap().reasoning_tokens, Some(2));
        let mut items = sample_conversation();
        items.push(Item::Assistant(turn.native));
        items.push(Item::ToolResult {
            call_id: "a".into(),
            content: "{\"ok\":1}".into(),
            is_error: false,
        });
        let body = request(
            &profile(Some(7)),
            "sys",
            &items,
            &crate::tools::definitions(),
        );
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 7);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["encrypted_content"], "opaque");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "a");
        assert_eq!(body["tools"][0]["type"], "function");
        assert!(body["tools"][0]["parameters"].is_object());
    }
}
