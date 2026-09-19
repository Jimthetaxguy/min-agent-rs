//! Live-provider graduation test. Ignored by default: it sends real requests and may cost
//! money, so it runs only when an operator opts in explicitly:
//!
//! ```sh
//! MIN_AGENT_LIVE_CONFIG=path/to/connections.toml MIN_AGENT_LIVE_PROFILE=openrouter \
//!   cargo test --test live -- --ignored --nocapture
//! ```
//!
//! Only a synthetic temporary workspace is exposed. The test plants a fact two directories
//! deep so a tool-using model must list, search, or read to find it; it prints one JSON
//! compatibility-matrix row (profile, protocol, endpoint, model, stop, rounds, calls, usage).
use min_agent::{
    agent::{run, Budget, RunOptions, StopReason},
    config::Config,
    model::HttpModelClient,
    tools::Workspace,
    trace::Trace,
};
use serde_json::json;
use std::path::PathBuf;

#[test]
#[ignore = "sends real provider requests; set MIN_AGENT_LIVE_CONFIG and MIN_AGENT_LIVE_PROFILE"]
fn live_provider_graduation() {
    let config = PathBuf::from(
        std::env::var("MIN_AGENT_LIVE_CONFIG").expect("MIN_AGENT_LIVE_CONFIG is required"),
    );
    let profile =
        std::env::var("MIN_AGENT_LIVE_PROFILE").expect("MIN_AGENT_LIVE_PROFILE is required");
    let config = Config::load(&config).unwrap();
    let (connection, model) = config.resolve(&profile).unwrap();
    let client = HttpModelClient::new(connection, model).unwrap();

    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("docs/notes");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        temp.path().join("README.md"),
        "Synthetic fixture project.\n",
    )
    .unwrap();
    std::fs::write(
        nested.join("launch.md"),
        "The launch codename is HELIOTROPE-47.\n",
    )
    .unwrap();
    let workspace = Workspace::open(temp.path()).unwrap();

    let text_only = !model.native_tools;
    let options = RunOptions {
        text_only,
        budget: Budget::default(),
        meta: json!({"profile": profile, "live": true}),
    };
    let report = run(
        &client,
        &workspace,
        "What is the launch codename recorded somewhere in this project? Use the tools to find it and quote it exactly.",
        &options,
        &mut Trace::disabled(),
    )
    .unwrap();
    let row = json!({
        "profile": profile,
        "protocol": connection.protocol.to_string(),
        "endpoint": connection.endpoint().unwrap().as_str(),
        "model": model.model,
        "text_only": text_only,
        "stop": report.stop,
        "rounds": report.rounds,
        "model_attempts": report.model_attempts,
        "tool_calls": report.tool_calls,
        "tool_errors": report.tool_errors,
        "usage": report.usage,
        "elapsed_ms": report.elapsed_ms,
        "found_fact": report.answer.as_deref().is_some_and(|a| a.contains("HELIOTROPE-47")),
    });
    println!("{}", serde_json::to_string(&row).unwrap());
    assert_eq!(report.stop, StopReason::Completed, "{row}");
    if !text_only {
        assert!(
            report.tool_calls > 0,
            "tool-using profile answered without tools: {row}"
        );
        assert!(row["found_fact"].as_bool().unwrap(), "{row}");
    }
}
