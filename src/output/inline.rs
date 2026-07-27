//! Animated single-line renderer, drawn in place on **stderr**.
//!
//! # Why this is hand-rolled instead of ratatui
//!
//! The plan was `ratatui` with `Viewport::Inline`. It cannot be used here.
//!
//! `Viewport::Inline` has to know the current cursor row to carve out its region,
//! so it calls `Backend::get_cursor_position()`. `CrosstermBackend` implements
//! that via `crossterm::cursor::position()`, which — in
//! `crossterm-0.29.0/src/cursor/sys/unix.rs` — does this:
//!
//! ```text
//! let mut stdout = io::stdout();
//! stdout.write_all(b"\x1B[6n")?;
//! ```
//!
//! It hardcodes `io::stdout()` and ignores whatever writer the backend was built
//! with. Constructing the backend over stderr does not help. The result, verified
//! end to end, is that `utter gen > file` writes `\x1b[6n` ahead of the command,
//! so the shell function inserts an escape sequence into the user's input buffer.
//!
//! stdout carrying exactly the command and nothing else is the invariant the whole
//! tool rests on, so it wins over the choice of TUI library. What ratatui was
//! buying here was one animated line; that is cheaper to write directly than to
//! work around, and it drops a large dependency from a binary whose startup budget
//! is a product requirement.
//!
//! What this keeps from the original intent: inline (never the alternate screen,
//! which would wipe scrollback), stderr only, and nothing left behind on the
//! screen when it finishes.

use std::io::{IsTerminal, Write};

use anyhow::Result;

use crate::llm::CommandProposal;
use crate::risk::Risk;
use crate::scanner::ScanResult;

use super::format::{self, DIM};
use super::Renderer;

/// Carriage return, then erase to end of line. Redraws in place without
/// consuming a new line, so scrollback is untouched.
const ERASE_LINE: &str = "\r\x1b[K";

pub struct Inline {
    label: String,
    tick: usize,
    drawn: bool,
}

impl Inline {
    /// `None` when stderr is not a terminal, so the caller falls back to the
    /// plain renderer. Cursor control against a pipe would just emit noise.
    pub fn new() -> Option<Self> {
        if !std::io::stderr().is_terminal() {
            return None;
        }
        Some(Self {
            label: String::new(),
            tick: 0,
            drawn: false,
        })
    }

    /// A failed spinner write is swallowed by the caller — see `Renderer::tick`.
    /// Never writes to stdout.
    fn draw(&mut self) -> Result<()> {
        let mut err = std::io::stderr();
        write!(
            err,
            "{ERASE_LINE}{} {}",
            format::spinner_frame(self.tick),
            format::paint(true, DIM, &self.label)
        )?;
        err.flush()?;
        self.drawn = true;
        Ok(())
    }

    fn erase(&mut self) {
        if !self.drawn {
            return;
        }
        let mut err = std::io::stderr();
        let _ = write!(err, "{ERASE_LINE}");
        let _ = err.flush();
        self.drawn = false;
    }

    fn write_lines(&self, lines: &[String]) -> Result<()> {
        let mut err = std::io::stderr();
        for line in lines {
            writeln!(err, "{line}")?;
        }
        err.flush()?;
        Ok(())
    }
}

/// Belt and braces: if anything unwinds between `start` and `stop`, the spinner
/// line is still erased rather than left stranded above the user's prompt.
impl Drop for Inline {
    fn drop(&mut self) {
        self.erase();
    }
}

impl Renderer for Inline {
    fn start(&mut self, label: &str) -> Result<()> {
        self.label = label.to_string();
        self.tick = 0;
        self.draw()
    }

    fn tick(&mut self) -> Result<()> {
        self.tick = self.tick.wrapping_add(1);
        self.draw()
    }

    fn stop(&mut self) -> Result<()> {
        self.erase();
        Ok(())
    }

    fn proposal(
        &mut self,
        proposal: &CommandProposal,
        scan: &ScanResult,
        effective: Risk,
    ) -> Result<()> {
        self.write_lines(&format::proposal_lines(proposal, scan, effective, true))
    }

    fn text(&mut self, body: &str) -> Result<()> {
        self.write_lines(&[body.to_string()])
    }

    fn note(&mut self, body: &str) -> Result<()> {
        self.write_lines(&[format::paint(true, DIM, body)])
    }
}
