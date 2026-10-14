#![forbid(unsafe_code)]
use anyhow::{Context, Result};
use min_agent::{
    agent::{run, Budget, RunOptions, StopReason},
    config::Config,
    model::HttpModelClient,
    tools::Workspace,
    trace::Trace,
};
use serde_json::json;
use std::{
    env, fmt,
    fs::OpenOptions,
    io::{self, BufWriter, IsTerminal},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

#[cfg_attr(test, derive(Debug))]
struct Cli {
    config: PathBuf,
    command: Command,
}

#[cfg_attr(test, derive(Debug))]
enum Command {
    Doctor {
        profile: Option<String>,
    },
    Ask {
        profile: String,
        workspace: PathBuf,
        text_only: bool,
        trace: Option<PathBuf>,
        report_json: bool,
        budget: BudgetArgs,
        prompt: String,
    },
}

/// Overrides for `Budget`; unset flags keep the library defaults.
#[cfg_attr(test, derive(Debug))]
struct BudgetArgs {
    max_rounds: Option<usize>,
    max_calls: Option<usize>,
    timeout_secs: Option<u64>,
    request_timeout_secs: Option<u64>,
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

#[cfg_attr(test, derive(Debug))]
enum Parsed {
    Run(Cli),
    Help(String),
    Version,
}

#[cfg_attr(test, derive(Debug))]
struct Usage {
    error: String,
    tip: Option<String>,
    usage: &'static str,
}

impl Usage {
    fn new(error: impl Into<String>, usage: &'static str) -> Self {
        Self {
            error: error.into(),
            tip: None,
            usage,
        }
    }

    fn tip(mut self, tip: impl Into<String>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    fn eprint(&self) {
        let color = color_enabled(&io::stderr());
        let mut out = String::new();
        out.push_str(&paint(color, "1;31", "error:"));
        out.push(' ');
        out.push_str(&self.error);
        if let Some(tip) = &self.tip {
            out.push_str("\n\n  ");
            out.push_str(&paint(color, "32", "tip:"));
            out.push(' ');
            out.push_str(tip);
        }
        out.push_str("\n\n");
        out.push_str(&paint(color, "1;4", "Usage:"));
        out.push(' ');
        out.push_str(&paint(color, "1", self.usage));
        out.push_str("\n\nFor more information, try '");
        out.push_str(&paint(color, "1", "--help"));
        out.push_str("'.\n");
        eprint!("{out}");
    }
}

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error: {}", self.error)
    }
}

const ROOT_USAGE: &str = "min-agent --config <CONFIG> <COMMAND>";
const ASK_USAGE: &str = "min-agent --config <CONFIG> ask [OPTIONS] --profile <PROFILE> <PROMPT>";
const DOCTOR_USAGE: &str = "min-agent --config <CONFIG> doctor [OPTIONS]";

const ASK_FLAGS: &[&str] = &[
    "--profile",
    "--workspace",
    "--text-only",
    "--trace",
    "--report-json",
    "--max-rounds",
    "--max-calls",
    "--timeout-secs",
    "--request-timeout-secs",
    "--max-retries",
    "--help",
];
const DOCTOR_FLAGS: &[&str] = &["--profile", "--help"];
const GLOBAL_FLAGS: &[&str] = &["--config", "--help", "--version"];
const COMMANDS: &[&str] = &["doctor", "ask", "help"];

fn color_enabled(stream: &impl IsTerminal) -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }
    let force = env::var_os("CLICOLOR_FORCE");
    if force.as_ref().is_some_and(|v| v != "0") {
        return true;
    }
    if env::var("CLICOLOR").ok().as_deref() == Some("0")
        || env::var("TERM").ok().as_deref() == Some("dumb")
    {
        return false;
    }
    let _ = force;
    stream.is_terminal()
}

fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    } else {
        text.to_string()
    }
}

fn help_text(color: bool, command: Option<&str>) -> String {
    match command {
        Some("ask") => ask_help(color),
        Some("doctor") => doctor_help(color),
        _ => root_help(color),
    }
}

fn root_help(color: bool) -> String {
    let usage = paint(color, "1;4", "Usage:");
    let bin = paint(color, "1", "min-agent");
    let config_slot = paint(color, "1", "--config <CONFIG>");
    let commands = paint(color, "1;4", "Commands:");
    let doctor = paint(color, "1", "doctor");
    let ask = paint(color, "1", "ask");
    let help = paint(color, "1", "help");
    let options = paint(color, "1;4", "Options:");
    let config = paint(color, "1", "--config");
    let short_h = paint(color, "1", "-h");
    let long_h = paint(color, "1", "--help");
    let short_v = paint(color, "1", "-V");
    let long_v = paint(color, "1", "--version");
    format!(
        "\
Minimal read-only Rust coding agent

{usage} {bin} {config_slot} <COMMAND>

{commands}
  {doctor}  Resolve explicit configuration and check credentials without network calls
  {ask}     Ask a model, optionally using bounded read-only workspace tools
  {help}    Print this message or the help of the given subcommand(s)

{options}
      {config} <CONFIG>
  {short_h}, {long_h}             Print help
  {short_v}, {long_v}          Print version
"
    )
}

fn doctor_help(color: bool) -> String {
    let usage = paint(color, "1;4", "Usage:");
    let bin = paint(color, "1", "min-agent");
    let config_slot = paint(color, "1", "--config <CONFIG>");
    let options = paint(color, "1;4", "Options:");
    let profile = paint(color, "1", "--profile");
    let short_h = paint(color, "1", "-h");
    let long_h = paint(color, "1", "--help");
    format!(
        "\
Resolve explicit configuration and check credentials without network calls

{usage} {bin} {config_slot} doctor [OPTIONS]

{options}
      {profile} <PROFILE>
  {short_h}, {long_h}               Print help
"
    )
}

fn ask_help(color: bool) -> String {
    let usage = paint(color, "1;4", "Usage:");
    let bin = paint(color, "1", "min-agent");
    let config_slot = paint(color, "1", "--config <CONFIG>");
    let profile_slot = paint(color, "1", "--profile <PROFILE>");
    let arguments = paint(color, "1;4", "Arguments:");
    let options = paint(color, "1;4", "Options:");
    let profile = paint(color, "1", "--profile");
    let workspace = paint(color, "1", "--workspace");
    let text_only = paint(color, "1", "--text-only");
    let trace = paint(color, "1", "--trace");
    let report = paint(color, "1", "--report-json");
    let rounds = paint(color, "1", "--max-rounds");
    let calls = paint(color, "1", "--max-calls");
    let timeout = paint(color, "1", "--timeout-secs");
    let request = paint(color, "1", "--request-timeout-secs");
    let retries = paint(color, "1", "--max-retries");
    let short_h = paint(color, "1", "-h");
    let long_h = paint(color, "1", "--help");
    format!(
        "\
Ask a model, optionally using bounded read-only workspace tools

{usage} {bin} {config_slot} ask [OPTIONS] {profile_slot} <PROMPT>

{arguments}
  <PROMPT>

{options}
      {profile} <PROFILE>
      {workspace} <WORKSPACE>
          [default: .]
      {text_only}
      {trace} <TRACE>
          Append a JSONL run trace (metadata only, no file contents) to this file
      {report}
          Print the run report as JSON to stderr instead of a one-line summary
      {rounds} <MAX_ROUNDS>
      {calls} <MAX_CALLS>
      {timeout} <TIMEOUT_SECS>
          Whole-run deadline in seconds
      {request} <REQUEST_TIMEOUT_SECS>
          Per-model-request timeout in seconds (capped by the remaining run time)
      {retries} <MAX_RETRIES>
          Retries of transient provider failures per round
  {short_h}, {long_h}
          Print help
"
    )
}

fn parse_env() -> Result<Parsed, Usage> {
    let args: Vec<String> = env::args().skip(1).collect();
    let color = color_enabled(&io::stdout());
    parse_args(&args, color)
}

fn parse_args(args: &[String], color: bool) -> Result<Parsed, Usage> {
    let mut index = 0;
    let mut config: Option<PathBuf> = None;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            break;
        }
        let (name, inline) = split_flag(arg);
        if name == "-h" || name == "--help" {
            return Ok(Parsed::Help(root_help(color)));
        }
        if name == "-V" || name == "--version" {
            return Ok(Parsed::Version);
        }
        if name == "--config" {
            index += 1;
            let value = require_value(args, &mut index, "--config <CONFIG>", inline)?;
            config = Some(PathBuf::from(value));
            continue;
        }
        if name.starts_with('-') {
            return Err(unexpected(name, GLOBAL_FLAGS, ROOT_USAGE));
        }
        index += 1;
        return parse_command(name, args, &mut index, config, color);
    }
    Err(Usage::new(
        "'min-agent' requires a subcommand but one was not provided\n  [subcommands: doctor, ask, help]",
        ROOT_USAGE,
    ))
}

fn parse_command(
    name: &str,
    args: &[String],
    index: &mut usize,
    mut config: Option<PathBuf>,
    color: bool,
) -> Result<Parsed, Usage> {
    if name == "help" {
        let topic = args.get(*index).map(String::as_str);
        if let Some(topic) = topic {
            if !COMMANDS.contains(&topic) && topic != "--help" {
                return Err(unknown_command(topic));
            }
        }
        let which = match topic {
            Some("ask") => Some("ask"),
            Some("doctor") => Some("doctor"),
            _ => None,
        };
        return Ok(Parsed::Help(help_text(color, which)));
    }
    if name != "doctor" && name != "ask" {
        return Err(unknown_command(name));
    }
    let mut profile: Option<String> = None;
    let mut workspace = PathBuf::from(".");
    let mut text_only = false;
    let mut trace = None;
    let mut report_json = false;
    let mut budget = BudgetArgs {
        max_rounds: None,
        max_calls: None,
        timeout_secs: None,
        request_timeout_secs: None,
        max_retries: None,
    };
    let mut prompt: Option<String> = None;
    let flags: &[&str] = if name == "ask" {
        ASK_FLAGS
    } else {
        DOCTOR_FLAGS
    };
    let usage = if name == "ask" {
        ASK_USAGE
    } else {
        DOCTOR_USAGE
    };
    let mut ended = false;
    while *index < args.len() {
        let arg = &args[*index];
        if !ended && arg == "--" {
            ended = true;
            *index += 1;
            continue;
        }
        if !ended && arg.starts_with('-') {
            let (flag, inline) = split_flag(arg);
            if flag == "-h" || flag == "--help" {
                return Ok(Parsed::Help(help_text(color, Some(name))));
            }
            if flag == "-V" || flag == "--version" {
                return Ok(Parsed::Version);
            }
            if flag == "--config" {
                *index += 1;
                let value = require_value(args, index, "--config <CONFIG>", inline)?;
                config = Some(PathBuf::from(value));
                continue;
            }
            *index += 1;
            if name == "ask" {
                match flag {
                    "--profile" => {
                        profile = Some(require_value(args, index, "--profile <PROFILE>", inline)?);
                    }
                    "--workspace" => {
                        workspace = PathBuf::from(require_value(
                            args,
                            index,
                            "--workspace <WORKSPACE>",
                            inline,
                        )?);
                    }
                    "--text-only" => {
                        reject_inline(flag, inline)?;
                        text_only = true;
                    }
                    "--trace" => {
                        trace = Some(PathBuf::from(require_value(
                            args,
                            index,
                            "--trace <TRACE>",
                            inline,
                        )?));
                    }
                    "--report-json" => {
                        reject_inline(flag, inline)?;
                        report_json = true;
                    }
                    "--max-rounds" => {
                        budget.max_rounds = Some(parse_number(
                            "--max-rounds <MAX_ROUNDS>",
                            &require_value(args, index, "--max-rounds <MAX_ROUNDS>", inline)?,
                        )?);
                    }
                    "--max-calls" => {
                        budget.max_calls = Some(parse_number(
                            "--max-calls <MAX_CALLS>",
                            &require_value(args, index, "--max-calls <MAX_CALLS>", inline)?,
                        )?);
                    }
                    "--timeout-secs" => {
                        budget.timeout_secs = Some(parse_number(
                            "--timeout-secs <TIMEOUT_SECS>",
                            &require_value(args, index, "--timeout-secs <TIMEOUT_SECS>", inline)?,
                        )?);
                    }
                    "--request-timeout-secs" => {
                        budget.request_timeout_secs = Some(parse_number(
                            "--request-timeout-secs <REQUEST_TIMEOUT_SECS>",
                            &require_value(
                                args,
                                index,
                                "--request-timeout-secs <REQUEST_TIMEOUT_SECS>",
                                inline,
                            )?,
                        )?);
                    }
                    "--max-retries" => {
                        budget.max_retries = Some(parse_number(
                            "--max-retries <MAX_RETRIES>",
                            &require_value(args, index, "--max-retries <MAX_RETRIES>", inline)?,
                        )?);
                    }
                    _ => return Err(unexpected(flag, flags, usage)),
                }
            } else if flag == "--profile" {
                profile = Some(require_value(args, index, "--profile <PROFILE>", inline)?);
            } else {
                return Err(unexpected(flag, flags, usage));
            }
            continue;
        }
        if name == "doctor" {
            return Err(unexpected_positional(arg));
        }
        if prompt.is_some() {
            return Err(Usage::new(
                format!("unexpected argument '{arg}' found"),
                ASK_USAGE,
            ));
        }
        prompt = Some(arg.clone());
        *index += 1;
    }
    let config = config.ok_or_else(|| {
        Usage::new(
            "the following required arguments were not provided:\n  --config <CONFIG>",
            ROOT_USAGE,
        )
    })?;
    let command = if name == "doctor" {
        Command::Doctor { profile }
    } else {
        let profile = profile.ok_or_else(|| {
            Usage::new(
                "the following required arguments were not provided:\n  --profile <PROFILE>",
                ASK_USAGE,
            )
        })?;
        let prompt = prompt.ok_or_else(|| {
            Usage::new(
                "the following required arguments were not provided:\n  <PROMPT>",
                ASK_USAGE,
            )
        })?;
        Command::Ask {
            profile,
            workspace,
            text_only,
            trace,
            report_json,
            budget,
            prompt,
        }
    };
    Ok(Parsed::Run(Cli { config, command }))
}

fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (arg, None),
    }
}

fn require_value(
    args: &[String],
    index: &mut usize,
    label: &str,
    inline: Option<&str>,
) -> Result<String, Usage> {
    if let Some(value) = inline {
        return Ok(value.to_string());
    }
    if *index >= args.len() || args[*index] == "--" {
        return Err(Usage::new(
            format!("a value is required for '{label}' but none was supplied"),
            ROOT_USAGE,
        ));
    }
    let value = args[*index].clone();
    *index += 1;
    Ok(value)
}

fn reject_inline(flag: &str, inline: Option<&str>) -> Result<(), Usage> {
    if inline.is_some() {
        Err(Usage::new(
            format!("unexpected value for '{flag}'"),
            ASK_USAGE,
        ))
    } else {
        Ok(())
    }
}

fn parse_number<T: std::str::FromStr>(label: &str, value: &str) -> Result<T, Usage>
where
    T::Err: fmt::Display,
{
    value.parse().map_err(|error| {
        Usage::new(
            format!("invalid value '{value}' for '{label}': {error}"),
            ASK_USAGE,
        )
    })
}

fn unexpected(flag: &str, options: &[&str], usage: &'static str) -> Usage {
    let mut error = Usage::new(format!("unexpected argument '{flag}' found"), usage);
    if let Some(similar) = suggest(flag, options) {
        error = error.tip(format!("a similar argument exists: '{similar}'"));
    }
    error
}

fn unknown_command(name: &str) -> Usage {
    let mut error = Usage::new(format!("unrecognized subcommand '{name}'"), ROOT_USAGE);
    if let Some(similar) = suggest(name, COMMANDS) {
        error = error.tip(format!("a similar subcommand exists: '{similar}'"));
    }
    error
}

fn unexpected_positional(arg: &str) -> Usage {
    Usage::new(format!("unexpected argument '{arg}' found"), DOCTOR_USAGE)
}

fn suggest<'a>(unknown: &str, options: &[&'a str]) -> Option<&'a str> {
    let unknown = unknown.trim_start_matches('-');
    let mut best: Option<(&str, usize)> = None;
    for option in options {
        let name = option.trim_start_matches('-');
        if name == unknown {
            continue;
        }
        let distance = edit_distance(unknown, name);
        if distance == 0 || distance > 2 || distance * 2 > name.len() {
            continue;
        }
        if best.is_none_or(|(_, current)| distance < current) {
            best = Some((*option, distance));
        }
    }
    best.map(|(name, _)| name)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut prev: Vec<usize> = (0..=right.len()).collect();
    let mut next = vec![0; right.len() + 1];
    for (i, ca) in left.chars().enumerate() {
        next[0] = i + 1;
        for (j, cb) in right.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            next[j + 1] = (prev[j + 1] + 1).min(next[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut next);
    }
    prev[right.len()]
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
    let cli = match parse_env() {
        Ok(Parsed::Run(cli)) => cli,
        Ok(Parsed::Help(text)) => {
            println!("{text}");
            return Ok(0);
        }
        Ok(Parsed::Version) => {
            println!("min-agent {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Err(error) => {
            error.eprint();
            return Ok(2);
        }
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    #[test]
    fn help_version_and_suggestions() {
        let help = parse_args(&args(&["--help"]), false).unwrap();
        assert!(
            matches!(help, Parsed::Help(text) if text.contains("doctor") && text.contains("--config"))
        );
        assert!(
            matches!(parse_args(&args(&["ask", "--help"]), false).unwrap(), Parsed::Help(text) if text.contains("--request-timeout-secs"))
        );
        assert!(matches!(
            parse_args(&args(&["--version"]), false).unwrap(),
            Parsed::Version
        ));
        let typo = parse_args(
            &args(&["--config", "c", "ask", "--profle", "p", "hi"]),
            false,
        )
        .unwrap_err();
        assert!(typo.tip.unwrap().contains("--profile"), "{}", typo.error);
        let command = parse_args(&args(&["docter"]), false).unwrap_err();
        assert!(command.tip.unwrap().contains("doctor"));
    }

    #[test]
    fn parses_ask_flags_and_required_arguments() {
        let parsed = parse_args(
            &args(&[
                "--config=app.toml",
                "ask",
                "--profile",
                "local",
                "--workspace",
                "ws",
                "--text-only",
                "--max-rounds",
                "4",
                "--max-retries=1",
                "--",
                "--not-a-flag",
            ]),
            false,
        )
        .unwrap();
        match parsed {
            Parsed::Run(cli) => {
                assert_eq!(cli.config, PathBuf::from("app.toml"));
                match cli.command {
                    Command::Ask {
                        profile,
                        workspace,
                        text_only,
                        budget,
                        prompt,
                        ..
                    } => {
                        assert_eq!(profile, "local");
                        assert_eq!(workspace, PathBuf::from("ws"));
                        assert!(text_only);
                        assert_eq!(budget.max_rounds, Some(4));
                        assert_eq!(budget.max_retries, Some(1));
                        assert_eq!(prompt, "--not-a-flag");
                    }
                    Command::Doctor { .. } => panic!("doctor"),
                }
            }
            _ => panic!("expected a run"),
        }
        let missing = parse_args(&args(&["ask", "--profile", "p"]), false).unwrap_err();
        assert!(missing.error.contains("--config"));
        let bad = parse_args(
            &args(&[
                "--config",
                "c",
                "ask",
                "--profile",
                "p",
                "--max-rounds",
                "nope",
                "hi",
            ]),
            false,
        )
        .unwrap_err();
        assert!(bad.error.contains("invalid value 'nope'"));
    }
}
