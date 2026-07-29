// A few helpers are exercised only by tests until step 7 wires them in (inline
// viewport). Drop this allow once that lands.
#![allow(dead_code)]

mod cli;
mod commands;
mod config;
mod context;
mod conversation;
mod error;
mod llm;
mod output;
mod piped;
mod prompt;
mod risk;
mod scanner;
mod session;
mod shell;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use config::{Config, Paths};
use session::SessionKey;

/// `current_thread`: one HTTP request per invocation and no concurrency, so the
/// multi-thread runtime would only add startup cost. Startup has to be
/// imperceptible — the model call should be the sole perceptible latency.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // `{:#}` prints the whole anyhow context chain. Errors go to stderr so a
            // failed run inserts nothing into the shell buffer.
            eprintln!("utter: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve()?;
    let cfg = Config::load(&paths, &cli.global.overrides())?;

    // `--clear` is terminal and applies whatever the subcommand: it is about this
    // terminal's stored state, not about generating anything.
    if cli.global.clear {
        let key = SessionKey::resolve();
        let (session_removed, state_removed) = session::clear(&paths, &key)?;
        match (session_removed, state_removed) {
            (false, false) => eprintln!("nothing stored for this shell"),
            _ => eprintln!(
                "cleared{}{}",
                if session_removed { " conversation" } else { "" },
                if state_removed {
                    " last-command state"
                } else {
                    ""
                }
            ),
        }
        return Ok(());
    }

    match &cli.command {
        Command::Gen(args) => commands::gen::run(args, &cli.global, &cfg, &paths).await,
        Command::Init(args) => commands::init::run(args, &paths),
        Command::Config => commands::config::run(&cfg, &paths),
    }
}
