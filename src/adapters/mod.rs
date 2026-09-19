//! Wire adapters. Each maps the neutral conversation to one protocol's request shape and
//! validates that protocol's response into a `ModelTurn`. `agent.rs` never branches on vendor.
use crate::{
    config::{ModelProfile, Protocol},
    model::{invalid, Item, ModelError, ModelTurn, ToolCall, ToolSpec},
};
use serde_json::Value;
use std::collections::HashSet;

pub mod chat;
pub mod messages;
pub mod responses;

pub fn request(
    protocol: Protocol,
    profile: &ModelProfile,
    system: &str,
    items: &[Item],
    tools: &[ToolSpec],
) -> Value {
    match protocol {
        Protocol::OpenaiChat => chat::request(profile, system, items, tools),
        Protocol::OpenaiResponses => responses::request(profile, system, items, tools),
        Protocol::AnthropicMessages => messages::request(profile, system, items, tools),
    }
}

pub fn parse(protocol: Protocol, response: Value) -> Result<ModelTurn, ModelError> {
    match protocol {
        Protocol::OpenaiChat => chat::parse(response),
        Protocol::OpenaiResponses => responses::parse(response),
        Protocol::AnthropicMessages => messages::parse(response),
    }
}

/// Parses a JSON-encoded arguments string; it must decode to an object.
fn string_arguments(raw: Option<&Value>) -> Result<Value, ModelError> {
    let Some(text) = raw.and_then(Value::as_str) else {
        return invalid("missing arguments string");
    };
    let arguments: Value =
        serde_json::from_str(text).or_else(|_| invalid("malformed tool arguments"))?;
    object_arguments(arguments)
}

fn object_arguments(arguments: Value) -> Result<Value, ModelError> {
    if arguments.is_object() {
        Ok(arguments)
    } else {
        invalid("tool arguments must be an object")
    }
}

fn non_empty<'a>(value: &'a Value, what: &str) -> Result<&'a str, ModelError> {
    match value.as_str().filter(|s| !s.is_empty()) {
        Some(s) => Ok(s),
        None => invalid(what),
    }
}

/// Checks invariants shared by every protocol once calls and text are extracted.
fn finish(
    native: Value,
    text: String,
    calls: Vec<ToolCall>,
    wants_tools: bool,
    usage: Option<crate::model::Usage>,
) -> Result<ModelTurn, ModelError> {
    let mut ids = HashSet::new();
    if !calls.iter().all(|call| ids.insert(call.id.as_str())) {
        return invalid("duplicate tool call ID");
    }
    if wants_tools == calls.is_empty() {
        return invalid("completion reason and tool calls disagree");
    }
    if calls.is_empty() && text.trim().is_empty() {
        return invalid("empty final answer");
    }
    Ok(ModelTurn {
        native,
        text,
        calls,
        usage,
    })
}

fn tokens(value: &Value) -> Option<u64> {
    value.as_u64()
}

#[cfg(test)]
pub(crate) mod conformance {
    //! Shared protocol-conformance contract: every adapter must supply a fixture for each
    //! named case, and each fixture must produce the expected outcome.
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum Expect {
        Calls(usize),
        Final,
        Invalid,
        Provider,
    }

    pub const REQUIRED: &[&str] = &[
        "single_call",
        "multiple_calls",
        "final_text",
        "opaque_reasoning_preserved",
        "refusal",
        "truncated",
        "empty_final",
        "missing_call_id",
        "malformed_arguments",
        "array_arguments",
        "duplicate_ids",
        "error_envelope",
        "reason_mismatch",
        "unknown_item",
        "usage_reported",
    ];

    pub fn check(protocol: Protocol, fixtures: Vec<(&'static str, Value, Expect)>) {
        let names: HashSet<_> = fixtures.iter().map(|f| f.0).collect();
        for required in REQUIRED {
            assert!(names.contains(required), "{protocol}: missing {required}");
        }
        for (name, response, expect) in fixtures {
            let got = match parse(protocol, response) {
                Ok(turn) if turn.calls.is_empty() => Expect::Final,
                Ok(turn) => Expect::Calls(turn.calls.len()),
                Err(ModelError::Invalid(_)) => Expect::Invalid,
                Err(ModelError::Provider(_)) => Expect::Provider,
                Err(ModelError::RequestTooLarge) => panic!("{protocol} {name}: unexpected"),
            };
            assert_eq!(got, expect, "{protocol} {name}");
        }
    }

    pub fn sample_conversation() -> Vec<Item> {
        vec![Item::User("question".into())]
    }

    pub fn profile(max: Option<u32>) -> ModelProfile {
        ModelProfile {
            connection: "c".into(),
            model: "m".into(),
            native_tools: true,
            max_output_tokens: max,
            output_limit_parameter: None,
        }
    }
}
