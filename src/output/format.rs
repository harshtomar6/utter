//! Pure formatting for everything the user reads. No IO, no terminal state.
//!
//! Both renderers share this so the risk banner cannot drift between the plain
//! path and the inline viewport, and so the wording is testable without a
//! terminal attached.

use std::ops::Range;

use crate::llm::CommandProposal;
use crate::risk::Risk;
use crate::scanner::ScanResult;

pub const RESET: &str = "\x1b[0m";
pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";
pub const INVERSE: &str = "\x1b[7m";

pub fn paint(color: bool, code: &str, body: &str) -> String {
    if color {
        format!("{code}{body}{RESET}")
    } else {
        body.to_string()
    }
}

/// The command with the offending fragment called out.
///
/// Inverse video on a terminal; a caret underline otherwise, so the highlight
/// survives being piped into a file or a log.
pub fn highlight(command: &str, span: &Range<usize>, color: bool) -> String {
    // Defend against a span that does not land on char boundaries rather than
    // slicing and panicking.
    let (Some(before), Some(mid), Some(after)) = (
        command.get(..span.start),
        command.get(span.clone()),
        command.get(span.end..),
    ) else {
        return command.to_string();
    };

    if color {
        return format!("{before}{INVERSE}{mid}{RESET}{after}");
    }
    let pad = " ".repeat(before.chars().count());
    let marks = "^".repeat(mid.chars().count().max(1));
    format!("{command}\n  {pad}{marks}")
}

/// Lines to print for a proposal, most-significant first.
///
/// Asymmetric friction is encoded here: `safe` gets one quiet line, `danger` gets
/// a banner. Approval fatigue is the real failure mode of this category — if every
/// command shouts, the shouting stops meaning anything.
pub fn proposal_lines(
    proposal: &CommandProposal,
    scan: &ScanResult,
    effective: Risk,
    color: bool,
) -> Vec<String> {
    let mut lines = Vec::new();

    match effective {
        Risk::Safe => {
            if !proposal.description.is_empty() {
                lines.push(paint(color, DIM, &proposal.description));
            }
        }
        Risk::Caution => {
            lines.push(format!(
                "{} {}",
                paint(color, effective.ansi(), "caution:"),
                proposal.description
            ));
            if let Some(worst) = scan.worst() {
                lines.push(paint(color, DIM, &format!("  {}", worst.note)));
            }
        }
        Risk::Danger => {
            lines.push(String::new());
            lines.push(paint(color, effective.ansi(), "!! DANGER"));
            match scan.worst() {
                Some(worst) => {
                    lines.push(format!("   {}", paint(color, BOLD, worst.note)));
                    lines.push(format!(
                        "   {}",
                        highlight(&proposal.command, &worst.span, color)
                    ));
                }
                // The model reported danger and the scanner found nothing specific
                // to point at. Still warn — the model saw something.
                None => {
                    lines.push(format!(
                        "   {}",
                        paint(color, BOLD, "reported as destructive")
                    ));
                    lines.push(format!("   {}", proposal.command));
                }
            }
            if !proposal.description.is_empty() {
                lines.push(format!("   {}", proposal.description));
            }
            lines.push(paint(
                color,
                DIM,
                "   nothing has run — read it before you press Enter",
            ));
            lines.push(String::new());
        }
    }

    if proposal.needs_output {
        // v1 records this and still hands the command over; the captured loop is
        // Phase 2.
        lines.push(paint(
            color,
            DIM,
            "  (the model wanted to read this command's output — run it and ask again)",
        ));
    }

    lines
}

/// Braille spinner. Rendered only into the transient inline viewport, never left
/// behind in scrollback.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner_frame(tick: usize) -> &'static str {
    SPINNER[tick % SPINNER.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner;

    fn proposal(command: &str, description: &str, risk: Risk) -> CommandProposal {
        CommandProposal {
            command: command.to_string(),
            description: description.to_string(),
            risk,
            needs_output: false,
        }
    }

    fn render(command: &str, description: &str, model_risk: Risk) -> Vec<String> {
        let p = proposal(command, description, model_risk);
        let scan = scanner::scan(command);
        let effective = scanner::effective_risk(model_risk, scan.risk);
        proposal_lines(&p, &scan, effective, false)
    }

    #[test]
    fn safe_gets_one_quiet_line() {
        let lines = render("ls -la", "Lists files.", Risk::Safe);
        assert_eq!(lines, vec!["Lists files."]);
    }

    #[test]
    fn safe_with_no_description_says_nothing_at_all() {
        assert!(render("ls", "", Risk::Safe).is_empty());
    }

    #[test]
    fn caution_labels_and_explains() {
        let lines = render(
            "git reset --hard HEAD~1",
            "Rewinds one commit.",
            Risk::Caution,
        );
        assert!(lines[0].starts_with("caution:"));
        assert!(lines[0].contains("Rewinds one commit."));
        assert!(lines[1].contains("discards uncommitted changes"));
    }

    #[test]
    fn a_tie_between_findings_resolves_deterministically() {
        // `rm -rf ./build` fires both `rm-force` and `rm-recursive` at Caution.
        // Ties break on rule name so the same command always shows the same note.
        let first = render("rm -rf ./build", "Removes the build dir.", Risk::Caution);
        let second = render("rm -rf ./build", "Removes the build dir.", Risk::Caution);
        assert_eq!(first, second);
        assert!(first[1].contains("deletes without prompting"));
    }

    #[test]
    fn danger_shows_a_banner_and_points_at_the_fragment() {
        let lines = render("rm -rf /", "Deletes everything.", Risk::Danger);
        let joined = lines.join("\n");
        assert!(joined.contains("!! DANGER"));
        assert!(joined.contains("filesystem root"));
        // Caret underline sits beneath the ` /` fragment.
        assert!(joined.contains("^^"));
        assert!(joined.contains("nothing has run"));
    }

    #[test]
    fn an_understated_risk_is_still_shown_as_danger() {
        // Model says safe, scanner disagrees. The banner must appear.
        let lines = render("rm -rf /", "Cleans up.", Risk::Safe);
        assert!(lines.join("\n").contains("!! DANGER"));
    }

    #[test]
    fn danger_with_no_scanner_finding_still_warns() {
        // Model claims danger, scanner has nothing to point at.
        let p = proposal("./deploy-to-prod.sh", "Deploys.", Risk::Danger);
        let scan = scanner::scan(&p.command);
        assert!(scan.findings.is_empty());
        let joined = proposal_lines(&p, &scan, Risk::Danger, false).join("\n");
        assert!(joined.contains("!! DANGER"));
        assert!(joined.contains("reported as destructive"));
        assert!(joined.contains("./deploy-to-prod.sh"));
    }

    #[test]
    fn needs_output_is_noted_without_blocking_the_command() {
        let mut p = proposal("ls", "Lists.", Risk::Safe);
        p.needs_output = true;
        let scan = scanner::scan("ls");
        let joined = proposal_lines(&p, &scan, Risk::Safe, false).join("\n");
        assert!(joined.contains("run it and ask again"));
    }

    #[test]
    fn no_ansi_escapes_when_colour_is_off() {
        for risk in [Risk::Safe, Risk::Caution, Risk::Danger] {
            let p = proposal("rm -rf /", "x", risk);
            let scan = scanner::scan(&p.command);
            let joined = proposal_lines(&p, &scan, risk, false).join("\n");
            assert!(!joined.contains('\x1b'), "{risk:?} leaked an escape code");
        }
    }

    #[test]
    fn ansi_escapes_appear_when_colour_is_on() {
        let p = proposal("rm -rf /", "x", Risk::Danger);
        let scan = scanner::scan(&p.command);
        let joined = proposal_lines(&p, &scan, Risk::Danger, true).join("\n");
        assert!(joined.contains(INVERSE), "fragment not inverse-highlighted");
        assert!(joined.contains(RESET));
    }

    #[test]
    fn highlight_survives_a_span_that_is_not_on_a_char_boundary() {
        // Byte 6 lands inside the two-byte é.
        assert_eq!(highlight("echo héllo", &(6..7), false), "echo héllo");
    }

    #[test]
    fn highlight_survives_an_out_of_range_span() {
        assert_eq!(highlight("ls", &(0..99), false), "ls");
    }

    #[test]
    fn spinner_cycles_without_panicking() {
        assert_eq!(spinner_frame(0), "⠋");
        assert_eq!(spinner_frame(SPINNER.len()), "⠋");
        assert_eq!(spinner_frame(usize::MAX).len(), 3);
    }
}
