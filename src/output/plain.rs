use std::io::{IsTerminal, Write};

use anyhow::Result;

use crate::llm::CommandProposal;
use crate::risk::Risk;
use crate::scanner::ScanResult;

use super::format::{self, DIM};
use super::Renderer;

/// Line-oriented stderr renderer.
///
/// Permanent, not a scaffold: it is the scriptable path, the CI path, and the path
/// used to iterate on the system prompt. It is also what runs whenever stderr is
/// not a terminal, so nothing here may depend on cursor control.
pub struct Plain {
    color: bool,
    spinner_visible: bool,
}

impl Plain {
    pub fn new() -> Self {
        Self {
            color: std::io::stderr().is_terminal(),
            spinner_visible: false,
        }
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

impl Default for Plain {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer for Plain {
    fn start(&mut self, label: &str) -> Result<()> {
        // Only draw for a human. When stderr is redirected this would be noise in
        // a log file.
        if !self.color {
            return Ok(());
        }
        let mut err = std::io::stderr();
        write!(err, "{}", format::paint(true, DIM, &format!("{label}…")))?;
        err.flush()?;
        self.spinner_visible = true;
        Ok(())
    }

    /// The plain renderer has no animation — the label sits there until the
    /// response lands.
    fn tick(&mut self) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if !self.spinner_visible {
            return Ok(());
        }
        // Carriage return + erase-to-end-of-line, so the finished output starts on
        // a clean line without scrolling anything away.
        let mut err = std::io::stderr();
        write!(err, "\r\x1b[K")?;
        err.flush()?;
        self.spinner_visible = false;
        Ok(())
    }

    fn proposal(
        &mut self,
        proposal: &CommandProposal,
        scan: &ScanResult,
        effective: Risk,
    ) -> Result<()> {
        self.write_lines(&format::proposal_lines(
            proposal, scan, effective, self.color,
        ))
    }

    fn text(&mut self, body: &str) -> Result<()> {
        self.write_lines(&[body.to_string()])
    }

    fn note(&mut self, body: &str) -> Result<()> {
        self.write_lines(&[format::paint(self.color, DIM, body)])
    }
}
