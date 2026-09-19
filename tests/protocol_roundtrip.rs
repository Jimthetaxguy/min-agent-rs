//! Actual HTTP exchanges against a loopback fake server, one per protocol adapter, plus
//! hostile-provider transport cases. No credentials or network beyond 127.0.0.1.
use min_agent::{
    agent::{run, Budget, Limit, RunOptions, StopReason},
    config::{Auth, Connection, ModelProfile, Protocol},
    model::{HttpModelClient, ProviderFailure},
    tools::Workspace,
    trace::Trace,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    thread,
    time::Duration,
};

struct Request {
    head: String,
    body: Value,
}

fn read_request(stream: &mut TcpStream) -> Request {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        head.extend(byte);
        assert!(head.len() < 16_384);
    }
    let head = String::from_utf8(head).unwrap();
    let size: usize = head
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    Request {
        head,
        body: serde_json::from_slice(&body).unwrap(),
    }
}

fn json_response(body: &Value) -> Vec<u8> {
    let body = body.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Serves each scripted raw response to one connection; sends every request to the channel.
fn serve(responses: Vec<Vec<u8>>) -> (String, mpsc::Receiver<Request>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        for raw in responses {
            let (mut stream, _) = listener.accept().unwrap();
            tx.send(read_request(&mut stream)).unwrap();
            let _ = stream.write_all(&raw);
        }
    });
    (format!("http://{address}/v1"), rx, handle)
}

fn client(base_url: String, protocol: Protocol, auth: Auth) -> HttpModelClient {
    let connection = Connection {
        protocol,
        base_url,
        auth,
        proxy: None,
    };
    let profile = ModelProfile {
        connection: "test".into(),
        model: "fixture".into(),
        native_tools: true,
        max_output_tokens: Some(512),
        output_limit_parameter: None,
    };
    HttpModelClient::new(&connection, &profile).unwrap()
}

fn workspace() -> (tempfile::TempDir, Workspace) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("hello.txt"), "fixture contents").unwrap();
    let w = Workspace::open(temp.path()).unwrap();
    (temp, w)
}

fn ask(client: &HttpModelClient, w: &Workspace, budget: Budget) -> min_agent::agent::RunReport {
    let options = RunOptions {
        budget,
        ..RunOptions::default()
    };
    run(client, w, "Read the file", &options, &mut Trace::disabled()).unwrap()
}

#[test]
fn chat_round_trip() {
    let (url, rx, server) = serve(vec![
        json_response(
            &json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"reasoning_content":"opaque","tool_calls":[{"id":"call_test","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"hello.txt\"}"}}]}}]}),
        ),
        json_response(
            &json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"done"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}),
        ),
    ]);
    let (_t, w) = workspace();
    let report = ask(
        &client(url, Protocol::OpenaiChat, Auth::None),
        &w,
        Budget::default(),
    );
    assert_eq!(report.stop, StopReason::Completed);
    assert_eq!(report.answer.as_deref(), Some("done"));
    assert_eq!(report.tool_calls, 1);
    let first = rx.recv().unwrap();
    let second = rx.recv().unwrap();
    assert!(first.head.starts_with("POST /v1/chat/completions "));
    assert!(!first.head.to_lowercase().contains("authorization:"));
    assert_eq!(first.body["model"], "fixture");
    assert_eq!(first.body["tools"].as_array().unwrap().len(), 3);
    let messages = &second.body["messages"];
    assert_eq!(messages[2]["reasoning_content"], "opaque");
    assert_eq!(messages[3]["tool_call_id"], "call_test");
    assert!(messages[3]["content"]
        .as_str()
        .unwrap()
        .contains("fixture contents"));
    server.join().unwrap();
}

#[test]
fn responses_round_trip() {
    let (url, rx, server) = serve(vec![
        json_response(&json!({"status":"completed","output":[
            {"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque"},
            {"type":"function_call","id":"fc_1","call_id":"call_r","name":"read_file","arguments":"{\"path\":\"hello.txt\"}"}
        ]})),
        json_response(&json!({"status":"completed","output":[
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}
        ],"usage":{"input_tokens":3,"output_tokens":4,"output_tokens_details":{"reasoning_tokens":1}}})),
    ]);
    let (_t, w) = workspace();
    let report = ask(
        &client(url, Protocol::OpenaiResponses, Auth::None),
        &w,
        Budget::default(),
    );
    assert_eq!(report.stop, StopReason::Completed);
    assert_eq!(report.answer.as_deref(), Some("done"));
    let first = rx.recv().unwrap();
    let second = rx.recv().unwrap();
    assert!(first.head.starts_with("POST /v1/responses "));
    assert_eq!(first.body["store"], false);
    assert_eq!(first.body["max_output_tokens"], 512);
    let input = second.body["input"].as_array().unwrap();
    assert_eq!(input[1]["encrypted_content"], "opaque");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_r");
    assert!(input[3]["output"]
        .as_str()
        .unwrap()
        .contains("fixture contents"));
    server.join().unwrap();
}

#[test]
fn messages_round_trip_with_header_auth() {
    let env = "MIN_AGENT_TEST_ANTHROPIC_KEY_71613";
    std::env::set_var(env, "test-key-value");
    let (url, rx, server) = serve(vec![
        json_response(
            &json!({"type":"message","role":"assistant","stop_reason":"tool_use","content":[
                {"type":"thinking","thinking":"t","signature":"sig"},
                {"type":"tool_use","id":"toolu_1","name":"read_file","input":{"path":"hello.txt"}},
                {"type":"tool_use","id":"toolu_2","name":"read_file","input":{"path":"missing.txt"}}
            ]}),
        ),
        json_response(
            &json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":5,"output_tokens":6}}),
        ),
    ]);
    let (_t, w) = workspace();
    let auth = Auth::HeaderEnv {
        header: "x-api-key".into(),
        env: env.into(),
    };
    let report = ask(
        &client(url, Protocol::AnthropicMessages, auth),
        &w,
        Budget::default(),
    );
    assert_eq!(report.stop, StopReason::Completed);
    assert_eq!(report.tool_calls, 2);
    assert_eq!(report.tool_errors, 1);
    let first = rx.recv().unwrap();
    let second = rx.recv().unwrap();
    let head = first.head.to_lowercase();
    assert!(first.head.starts_with("POST /v1/messages "));
    assert!(head.contains("x-api-key: test-key-value"));
    assert!(head.contains("anthropic-version: 2023-06-01"));
    assert!(!head.contains("authorization:"));
    assert_eq!(first.body["max_tokens"], 512);
    let messages = second.body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["content"][0]["signature"], "sig");
    let results = messages[2]["content"].as_array().unwrap();
    assert_eq!(results[0]["tool_use_id"], "toolu_1");
    assert_eq!(results[0]["is_error"], false);
    assert_eq!(results[1]["is_error"], true);
    server.join().unwrap();
}

fn stop_for(raw: Vec<u8>, budget: Budget) -> StopReason {
    let (url, _rx, server) = serve(vec![raw]);
    let (_t, w) = workspace();
    let report = ask(&client(url, Protocol::OpenaiChat, Auth::None), &w, budget);
    server.join().unwrap();
    report.stop
}

fn no_retry() -> Budget {
    Budget {
        max_retries: 0,
        ..Budget::default()
    }
}

#[test]
fn chunked_oversize_body_without_length_is_cut_off() {
    let chunk = "a".repeat(65_536);
    let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    for _ in 0..20 {
        raw.extend(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes());
    }
    raw.extend(b"0\r\n\r\n");
    assert_eq!(
        stop_for(raw, no_retry()),
        StopReason::ProviderError {
            failure: ProviderFailure::BodyTooLarge,
            attempts: 1
        }
    );
}

#[test]
fn slow_body_is_bounded_by_the_request_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100000\r\n\r\n{\"choices\":");
        thread::sleep(Duration::from_secs(4));
    });
    let (_t, w) = workspace();
    let budget = Budget {
        request_timeout: Duration::from_secs(1),
        max_retries: 0,
        ..Budget::default()
    };
    let started = std::time::Instant::now();
    let report = ask(
        &client(
            format!("http://{address}/v1"),
            Protocol::OpenaiChat,
            Auth::None,
        ),
        &w,
        budget,
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        report.stop,
        StopReason::ProviderError {
            failure: ProviderFailure::Timeout,
            attempts: 1
        }
    );
    server.join().unwrap();
}

#[test]
fn redirects_are_not_followed_and_credentials_do_not_cross_origin() {
    let env = "MIN_AGENT_TEST_BEARER_33019";
    std::env::set_var(env, "secret-bearer");
    // A second origin that would receive the request if the redirect were followed.
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    other.set_nonblocking(true).unwrap();
    let target = format!("http://{}/steal", other.local_addr().unwrap());
    let raw = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let (url, rx, server) = serve(vec![raw.into_bytes()]);
    let (_t, w) = workspace();
    let auth = Auth::BearerEnv { env: env.into() };
    let report = ask(&client(url, Protocol::OpenaiChat, auth), &w, no_retry());
    server.join().unwrap();
    assert!(matches!(
        report.stop,
        StopReason::ProviderError {
            failure: ProviderFailure::Status { code: 307, .. },
            ..
        }
    ));
    assert!(rx
        .recv()
        .unwrap()
        .head
        .to_lowercase()
        .contains("authorization: bearer secret-bearer"));
    assert!(other.accept().is_err(), "redirect target was contacted");
}

#[test]
fn event_stream_reply_to_non_streaming_request_is_rejected() {
    let body = "data: {\"choices\":[]}\n\n";
    let raw = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    assert_eq!(
        stop_for(raw.into_bytes(), no_retry()),
        StopReason::ProviderError {
            failure: ProviderFailure::UnexpectedContentType,
            attempts: 1
        }
    );
}

#[test]
fn service_unavailable_is_retried_then_succeeds() {
    let busy = b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let (url, rx, server) = serve(vec![
        busy,
        json_response(
            &json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"done"}}]}),
        ),
    ]);
    let (_t, w) = workspace();
    let report = ask(
        &client(url, Protocol::OpenaiChat, Auth::None),
        &w,
        Budget::default(),
    );
    server.join().unwrap();
    assert_eq!(report.stop, StopReason::Completed);
    assert_eq!(report.model_attempts, 2);
    assert_eq!(rx.iter().count(), 2);
    // Usage absent from the provider stays unknown rather than zero.
    assert_eq!(report.usage.input_tokens, None);
}

#[test]
fn context_budget_stops_before_sending() {
    let (url, rx, server) = serve(vec![]);
    let (_t, w) = workspace();
    let options = RunOptions {
        budget: Budget {
            max_context_bytes: 4096,
            ..Budget::default()
        },
        ..RunOptions::default()
    };
    let prompt = "x".repeat(8000);
    let report = run(
        &client(url, Protocol::OpenaiChat, Auth::None),
        &w,
        &prompt,
        &options,
        &mut Trace::disabled(),
    )
    .unwrap();
    assert_eq!(
        report.stop,
        StopReason::BudgetExceeded {
            limit: Limit::ContextBytes
        }
    );
    server.join().unwrap();
    assert!(rx.try_recv().is_err());
}
