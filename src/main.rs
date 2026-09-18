use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use min_agent::{
    agent::{run, Budget},
    config::{endpoint, Config},
    model::ChatClient,
    tools::Workspace,
};
use std::path::PathBuf;

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
        prompt: String,
    },
}

fn execute() -> Result<()> {
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
                println!("profile={name}\nprotocol={}\nendpoint={}\nmodel={}\nnative_tools={}\nauth={}\nstatus=ready (offline; provider compatibility not tested)\n",
                    connection.protocol, endpoint(&connection.base_url)?, model.model, model.native_tools, connection.auth.description());
            }
        }
        Command::Ask {
            profile,
            workspace,
            text_only,
            prompt,
        } => {
            let (connection, model) = config.resolve(&profile)?;
            let client = ChatClient::new(connection, model)?;
            let workspace = Workspace::open(&workspace)?;
            eprintln!("Read-only workspace: {}\nModel endpoint: {}\nModel: {}\nWorkspace tool results may be sent to this endpoint.",
                workspace.display_path.display(), endpoint(&connection.base_url)?, model.model);
            let result = run(
                &client,
                &workspace,
                &prompt,
                text_only || !model.native_tools,
                Budget::default(),
            )
            .context("Run stopped")?;
            println!("{}", result.answer);
            eprintln!(
                "Completed: {} model rounds, {} tool calls",
                result.rounds, result.tool_calls
            );
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = execute() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
