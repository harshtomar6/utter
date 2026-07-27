//! Everything a human reads goes through here, and everything here goes to
//! **stderr**.
//!
//! stdout carries exactly one thing — the command — written once by
//! `commands::gen`. Keeping that split in one place is what lets the shell
//! function do `c=$(utter gen "$@")` without ever capturing UI text.

pub mod format;
pub mod inline;
pub mod plain;

use anyhow::Result;

use crate::llm::CommandProposal;
use crate::risk::Risk;
use crate::scanner::ScanResult;

pub trait Renderer {
    /// Called before the model request. May draw a spinner.
    fn start(&mut self, label: &str) -> Result<()>;
    /// Advance any animation. Driven by the caller's select loop so the renderer
    /// stays synchronous and owns no tasks.
    fn tick(&mut self) -> Result<()>;
    /// Called once the response arrives, before anything else is drawn.
    fn stop(&mut self) -> Result<()>;
    fn proposal(
        &mut self,
        proposal: &CommandProposal,
        scan: &ScanResult,
        effective: Risk,
    ) -> Result<()>;
    /// A plain-text answer with no command — a valid outcome, not an error.
    fn text(&mut self, body: &str) -> Result<()>;
    /// Secondary information: notes, hints, `--explain` narration.
    fn note(&mut self, body: &str) -> Result<()>;
}

/// Inline viewport for an interactive terminal, plain lines otherwise.
///
/// `Inline::new` returns `None` when stderr is not a TTY or the terminal cannot be
/// set up, and the plain renderer takes over. That path is a permanent
/// first-class feature, not a degraded mode: it is how the tool stays scriptable
/// and how prompts get iterated.
pub fn pick(force_plain: bool) -> Box<dyn Renderer> {
    if force_plain {
        return Box::new(plain::Plain::new());
    }
    match inline::Inline::new() {
        Some(inline) => Box::new(inline),
        None => Box::new(plain::Plain::new()),
    }
}
