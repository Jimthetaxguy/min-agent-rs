use min_agent::{
    agent::{run, Budget},
    config::{Auth, Connection, ModelProfile},
    model::ChatClient,
    tools::Workspace,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
};

#[test]
fn actual_http_tool_result_round_trip() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    let server = thread::spawn(move || {
        for turn in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                header.extend(byte);
                assert!(header.len() < 16_384);
            }
            let header = String::from_utf8(header).unwrap();
            assert!(header.starts_with("POST /v1/chat/completions "));
            assert!(!header.to_lowercase().contains("authorization:"));
            let size: usize = header
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; size];
            stream.read_exact(&mut body).unwrap();
            tx.send(serde_json::from_slice::<Value>(&body).unwrap())
                .unwrap();
            let message = if turn == 0 {
                json!({"role":"assistant","content":null,"reasoning_content":"opaque","tool_calls":[{"id":"call_test","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"hello.txt\"}"}}]})
            } else {
                json!({"role":"assistant","content":"done"})
            };
            let response = json!({"choices":[{"finish_reason":if turn == 0 {"tool_calls"} else {"stop"},"message":message}]}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        }
    });
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("hello.txt"), "fixture contents").unwrap();
    let connection = Connection {
        protocol: "openai_chat".into(),
        base_url: format!("http://{address}/v1"),
        auth: Auth::None,
    };
    let profile = ModelProfile {
        connection: "test".into(),
        model: "fixture".into(),
        native_tools: true,
        max_output_tokens: None,
        output_limit_parameter: None,
    };
    let client = ChatClient::new(&connection, &profile).unwrap();
    let result = run(
        &client,
        &Workspace::open(temp.path()).unwrap(),
        "Read the file",
        false,
        Budget::default(),
    )
    .unwrap();
    assert_eq!(result.answer, "done");
    assert_eq!(result.tool_calls, 1);
    let first = rx.recv().unwrap();
    let second = rx.recv().unwrap();
    assert_eq!(first["model"], "fixture");
    assert_eq!(first["tools"].as_array().unwrap().len(), 3);
    assert_eq!(second["messages"][2]["reasoning_content"], "opaque");
    assert_eq!(second["messages"][3]["tool_call_id"], "call_test");
    assert!(second["messages"][3]["content"]
        .as_str()
        .unwrap()
        .contains("fixture contents"));
    server.join().unwrap();
}
