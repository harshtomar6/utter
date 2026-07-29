//! Reading command output the user piped in.
//!
//! ```text
//! $ ps axo pid,rss,comm | sort -nrk2 | head | ask why is my ram full
//! ```
//!
//! This is the manual, human-gated half of `needs_output`: the user runs the
//! command, looks at the result, and decides what the model gets to see. No hook
//! captures anything and nothing is executed on the model's behalf.
//!
//! # This is untrusted input
//!
//! Whatever arrives here — log lines, HTTP responses, file contents — may have
//! been written by someone else, and may contain text shaped like instructions.
//! It is fenced and labelled as data in the prompt, and the model is told to
//! treat it as such. That framing is a mitigation, not a guarantee, which is the
//! standing argument for keeping a human between the model and execution.

use std::io::{IsTerminal, Read};

/// Reads piped stdin, or `None` when stdin is a terminal or carries nothing.
///
/// A terminal check comes first so an ordinary `ask do a thing` never blocks
/// waiting for input that is not coming.
pub fn read(limit: usize) -> Option<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }

    let mut raw = String::new();
    // A read error means we simply have no context to add — never a reason to
    // fail the request.
    stdin
        .take(hard_cap(limit) as u64)
        .read_to_string(&mut raw)
        .ok()?;

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_middle(trimmed, limit))
}

/// Read at most this much regardless, so `ask < /dev/urandom` cannot spool
/// forever before truncation happens.
fn hard_cap(limit: usize) -> usize {
    limit.saturating_mul(4).max(limit)
}

/// Keeps the head and tail, drops the middle.
///
/// Both ends matter: a command's first lines carry headers and its last lines
/// carry the summary or the error. Cutting only the tail throws away the half
/// that usually explains the problem.
pub fn truncate_middle(body: &str, limit: usize) -> String {
    if body.chars().count() <= limit {
        return body.to_string();
    }

    let keep = limit / 2;
    let head: String = body.chars().take(keep).collect();
    let tail: String = {
        let all: Vec<char> = body.chars().collect();
        all[all.len().saturating_sub(keep)..].iter().collect()
    };
    let dropped = body.chars().count() - head.chars().count() - tail.chars().count();

    // Trim to line boundaries so the model is not handed half a line.
    let head = head.rsplit_once('\n').map(|(h, _)| h).unwrap_or(&head);
    let tail = tail.split_once('\n').map(|(_, t)| t).unwrap_or(&tail);

    format!("{head}\n[... {dropped} characters omitted ...]\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_is_untouched() {
        assert_eq!(truncate_middle("a\nb\nc", 100), "a\nb\nc");
    }

    #[test]
    fn long_input_keeps_both_ends() {
        let body = (0..500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = truncate_middle(&body, 200);

        assert!(out.contains("line 0"), "head lost");
        assert!(out.contains("line 499"), "tail lost");
        assert!(out.contains("characters omitted"));
        assert!(out.chars().count() < body.chars().count());
    }

    #[test]
    fn truncation_reports_how_much_it_dropped() {
        let body = "x".repeat(1000);
        let out = truncate_middle(&body, 100);
        // Silently shortening someone's data would be worse than saying so.
        assert!(out.contains("omitted"));
    }

    #[test]
    fn truncation_is_char_safe_on_multibyte_input() {
        let body = "é".repeat(1000);
        let out = truncate_middle(&body, 100);
        assert!(out.contains("omitted"));
        assert!(out.starts_with('é'));
    }

    #[test]
    fn exactly_at_the_limit_is_not_truncated() {
        let body = "x".repeat(50);
        assert_eq!(truncate_middle(&body, 50), body);
    }

    #[test]
    fn hard_cap_never_reads_less_than_the_limit() {
        assert!(hard_cap(4000) >= 4000);
        assert!(hard_cap(0) == 0);
        assert!(hard_cap(usize::MAX) >= usize::MAX / 2);
    }
}
