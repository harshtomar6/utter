// Modules are wired in as they land; the CLI arrives with the commands.
// Re-exports and helpers land before their first consumer does.
#![allow(dead_code, unused_imports)]

mod config;
mod context;
mod conversation;
mod error;
mod llm;
mod output;
mod prompt;
mod risk;
mod scanner;
mod session;

use config::{Config, Overrides, Paths};

fn main() -> anyhow::Result<()> {
    let paths = Paths::resolve()?;
    let cfg = Config::load(&paths, &Overrides::default())?;
    println!("{cfg:?}");
    println!("{paths:?}");
    Ok(())
}
