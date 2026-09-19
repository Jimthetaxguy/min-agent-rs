#![forbid(unsafe_code)]
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use min_agent::{
    agent::{run, Budget, RunOptions, StopReason},
    config::Config,
    model::HttpModelClient,
    tools::Workspace,
    trace::Trace,
};
use serde_json::json;
use std::{fs::OpenOptions, io::BufWriter, path::PathBuf, process::ExitCode, time::Duration};

#[derive(Parser)]
#[command(version, about = "Minimal read-only Rust coding agent")]
struct Cli {
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Resolve explicit configuration and check credentials without network calls.
    Doctor {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Ask a model, optionally using bounded read-only workspace tools.
    Ask {
        #[arg(long)]
        profile: String,
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        text_only: bool,
        /// Append a JSONL run trace (metadata only, no file contents) to this file.
        #[arg(long)]
        trace: Option<PathBuf>,
        /// Print the run report as JSON to stderr instead of a one-line summary.
        #[arg(long)]
        report_json: bool,
        #[command(flatten)]
        budget: BudgetArgs,
        prompt: String,
    },
}

/// Overrides for `Budget`; unset flags keep the library defaults.
#[derive(Args)]
struct BudgetArgs {
    #[arg(long)]
    max_rounds: Option<usize>,
    #[arg(long)]
    max_calls: Option<usize>,
    /// Whole-run deadline in seconds.
    #[arg(long)]
    timeout_secs: Option<u64>,
    /// Per-model-request timeout in seconds (capped by the remaining run time).
    #[arg(long)]
    request_timeout_secs: Option<u64>,
    /// Retries of transient provider failures per round.
    #[arg(long)]
    max_retries: Option<u32>,
}

impl BudgetArgs {
    fn budget(&self) -> Budget {
        let mut b = Budget::default();
        if let Some(v) = self.max_rounds {
            b.max_rounds = v;
        }
        if let Some(v) = self.max_calls {
            b.max_calls = v;
        }
        if let Some(v) = self.timeout_secs {
            b.wall_clock = Duration::from_secs(v);
        }
        if let Some(v) = self.request_timeout_secs {
            b.request_timeout = Duration::from_secs(v);
        }
        if let Some(v) = self.max_retries {
            b.max_retries = v;
        }
        b
    }
}

/// 0 completed; 1 configuration or usage error; 2-5 a run that stopped without completing.
fn exit_code(stop: &StopReason) -> u8 {
    match stop {
        StopReason::Completed => 0,
        StopReason::BudgetExceeded { .. } => 2,
        StopReason::ProviderError { .. } => 3,
        StopReason::InvalidResponse { .. } => 4,
        StopReason::TraceFailed => 5,
    }
}

fn execute() -> Result<u8> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    match cli.command {
        Command::Doctor { profile } => {
            let profiles: Vec<_> = match profile {
                Some(name) => vec![name],
                None => config.models.keys().cloned().collect(),
            };
            for name in profiles {
                let (connection, model) = config.resolve(&name)?;
                connection.auth.credential()?;
                println!("profile={name}\nprotocol={}\nendpoint={}\nmodel={}\nnative_tools={}\nauth={}\nproxy={}\nfingerprint={}\nstatus=ready (offline; provider compatibility not tested)\n",
                    connection.protocol, connection.endpoint()?, model.model, model.native_tools,
                    connection.auth.description(), connection.proxy.as_deref().unwrap_or("none"),
                    connection.fingerprint());
            }
            Ok(0)
        }
        Command::Ask {
            profile,
            workspace,
            text_only,
            trace,
            report_json,
            budget,
            prompt,
        } => {
            let (connection, model) = config.resolve(&profile)?;
            let client = HttpModelClient::new(connection, model)?;
            let workspace = Workspace::open(&workspace)?;
            let endpoint = connection.endpoint()?;
            let options = RunOptions {
                text_only: text_only || !model.native_tools,
                budget: budget.budget(),
                meta: json!({
                    "profile": profile,
                    "protocol": connection.protocol.to_string(),
                    "endpoint": endpoint.as_str(),
                    "model": model.model,
                    "connection_fingerprint": connection.fingerprint(),
                    "agent_version": env!("CARGO_PKG_VERSION"),
                }),
            };
            let mut trace = match trace {
                Some(path) => {
                    let file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .context("Cannot open trace file")?;
                    Trace::to_writer(Box::new(BufWriter::new(file)))
                }
                None => Trace::disabled(),
            };
            eprintln!("Read-only workspace: {}\nModel endpoint: {}\nModel: {}\nWorkspace tool results may be sent to this endpoint.",
                workspace.display_path.display(), endpoint, model.model);
            let report = run(&client, &workspace, &prompt, &options, &mut trace)?;
            if let Some(answer) = &report.answer {
                println!("{answer}");
            }
            if report_json {
                eprintln!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.stop.is_completed() {
                eprintln!(
                    "Completed: {} model rounds, {} tool calls ({} errors), run {}",
                    report.rounds, report.tool_calls, report.tool_errors, report.run_id
                );
            } else {
                eprintln!(
                    "Run stopped: {} after {} model rounds, {} tool calls, run {}",
                    report.stop, report.rounds, report.tool_calls, report.run_id
                );
                if let Some(text) = &report.last_text {
                    eprintln!("Last model text (not a final answer):\n{text}");
                }
            }
            Ok(exit_code(&report.stop))
        }
    }
}

fn main() -> ExitCode {
    match execute() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(1)
        }
    }
}
