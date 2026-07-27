use std::fmt::Write as _;

use crate::context::ShellContext;

/// A command the user's shell recorded as having failed, used by bare `ask`.
///
/// Only `command`, `exit_code` and `cwd` — `preexec`/`precmd` hooks never see a
/// command's stdout (it went to the terminal, not to the shell), so there is no
/// captured output to pass here in v1. The exit code plus the command text is
/// enough to diagnose the overwhelming majority of failures.
#[derive(Debug, Clone)]
pub struct Failure<'a> {
    pub command: &'a str,
    pub exit_code: i32,
    pub cwd: Option<&'a str>,
    /// The shell refused to parse the line rather than running it.
    pub parse_error: bool,
}

#[derive(Debug, Clone)]
pub struct PromptInput<'a> {
    pub ctx: &'a ShellContext,
    /// `Some` puts the model in explain-and-fix mode. Wired up in step 6.
    pub failure: Option<Failure<'a>>,
    pub explain: bool,
}

/// Built fresh every invocation and never persisted to the session file: cwd,
/// shell and available tools all drift between runs, and a stale system prompt
/// produces commands for a machine state that no longer exists.
pub fn build(input: &PromptInput<'_>) -> String {
    let mut p = String::with_capacity(2048);

    p.push_str(
        "You translate natural-language requests into shell commands for one specific machine.\n\
         The command you produce is placed directly into the user's shell input buffer. They read \
         it and press Enter themselves — you never execute anything.\n\n",
    );

    p.push_str("# Machine\n");
    p.push_str(&input.ctx.render());
    p.push('\n');

    let _ = writeln!(
        p,
        "# {} flag rules (getting these wrong is the main source of broken commands)",
        input.ctx.flavor.label()
    );
    p.push_str(input.ctx.flavor.guidance());
    p.push_str("\n\n");

    p.push_str(
        "# How to answer\n\
         - Call `run_command` exactly once.\n\
         - Emit ONE command or pipeline. No newlines, no multi-line scripts, no here-docs.\n\
         - Use only tools listed as on PATH, plus shell builtins and POSIX standard utilities. \
         If the natural tool is missing, solve it with what is present.\n\
         - Paths are relative to the working directory above unless the user says otherwise.\n\
         - `description`: one short plain-language sentence — what it does and why.\n\
         - `risk`: `safe` = read-only. `caution` = recoverable modification. `danger` = \
         destructive, irreversible, or system-wide. Report this honestly; do not soften it. \
         An understated risk is worse than a wrong command, because the user trusts it.\n\
         - `needs_output`: `true` only if you would need to read this command's output before you \
         could answer. `false` for a self-contained command the user just runs.\n\
         - Never hide a destructive step behind a read-only one in the same pipeline.\n\
         - Prefer the reversible form when two commands are equivalent (`mv` to a temp path over \
         `rm`, `--dry-run` first where the tool supports it).\n\
         - If the request is ambiguous, take the most conservative reading and say which reading \
         you took in `description`.\n\
         - If no shell command can serve the request, reply in plain text instead of calling the \
         tool.\n",
    );

    if let Some(fail) = &input.failure {
        p.push_str("\n# The user's last command failed\n");
        let _ = writeln!(p, "Command: {}", fail.command);
        let _ = writeln!(p, "Exit code: {}", fail.exit_code);
        if let Some(cwd) = fail.cwd {
            let _ = writeln!(p, "Ran in: {cwd}");
        }
        if fail.parse_error {
            p.push_str(
                "\nThe shell could not PARSE this line — it never ran. The fault is in the \
                 text itself: quoting, escaping, an unbalanced quote or bracket, or a \
                 metacharacter that needed escaping (a bare `;` inside `find -exec` is the \
                 classic one). Do not look for a runtime cause.\n",
            );
        }
        p.push_str(
            "\nThe user invoked this tool with no request, which means: explain and fix that \
             failure.\n\
             - Begin `description` with the cause in a single clause, then what the fix changes.\n\
             - Propose the corrected command, not a diagnostic command, unless the cause genuinely \
             cannot be determined from the command text and exit code — in that case propose the \
             one command that would reveal it.\n\
             - You do not have the failed command's output. Reason from the command text and exit \
             code; do not claim to have seen an error message.\n",
        );
    }

    if input.explain {
        p.push_str(
            "\n# Verbose mode\n\
             The user asked for reasoning. Use `description` for the usual one sentence, and put \
             the longer explanation in your message text alongside the tool call.\n",
        );
    }

    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Flavor, ShellContext};

    fn ctx(flavor: Flavor) -> ShellContext {
        ShellContext {
            os: "macOS".into(),
            arch: "arm64".into(),
            flavor,
            shell: "/bin/zsh".into(),
            cwd: "/Users/u/dev".into(),
            tools: vec!["rg", "jq", "git"],
            gnu_prefixed: vec!["gsed"],
        }
    }

    fn input<'a>(c: &'a ShellContext, failure: Option<Failure<'a>>) -> PromptInput<'a> {
        PromptInput {
            ctx: c,
            failure,
            explain: false,
        }
    }

    #[test]
    fn prompt_carries_live_machine_context() {
        let c = ctx(Flavor::Bsd);
        let p = build(&input(&c, None));
        assert!(p.contains("/Users/u/dev"));
        assert!(p.contains("/bin/zsh"));
        assert!(p.contains("rg, jq, git"));
        assert!(p.contains("gsed"));
    }

    #[test]
    fn bsd_and_gnu_prompts_carry_different_flag_rules() {
        let bsd = build(&input(&ctx(Flavor::Bsd), None));
        let gnu = build(&input(&ctx(Flavor::Gnu), None));
        assert!(bsd.contains("ps axo"));
        assert!(gnu.contains("ps -eo"));
        assert!(bsd.contains("BSD flag rules"));
        assert!(gnu.contains("GNU flag rules"));
    }

    #[test]
    fn failure_block_is_absent_without_a_failure() {
        let c = ctx(Flavor::Bsd);
        assert!(!build(&input(&c, None)).contains("last command failed"));
    }

    #[test]
    fn failure_block_states_cause_first_and_disclaims_output() {
        let c = ctx(Flavor::Bsd);
        let p = build(&input(
            &c,
            Some(Failure {
                command: "tar -xf archive.tar.gz",
                exit_code: 1,
                cwd: Some("/tmp"),
                parse_error: false,
            }),
        ));
        assert!(p.contains("tar -xf archive.tar.gz"));
        assert!(p.contains("Exit code: 1"));
        assert!(p.contains("Ran in: /tmp"));
        // The model must not invent an error message it never received.
        assert!(p.contains("do not claim to have seen an error message"));
    }

    #[test]
    fn a_parse_error_tells_the_model_the_line_never_ran() {
        // Otherwise it hunts for a runtime cause that does not exist.
        let c = ctx(Flavor::Bsd);
        let p = build(&input(
            &c,
            Some(Failure {
                command: "find . -exec stat {} ; | sort",
                exit_code: 1,
                cwd: None,
                parse_error: true,
            }),
        ));
        assert!(p.contains("could not PARSE"));
        assert!(p.contains("Do not look for a runtime cause"));
    }

    #[test]
    fn explain_mode_adds_a_verbose_section() {
        let c = ctx(Flavor::Bsd);
        let mut i = input(&c, None);
        i.explain = true;
        assert!(build(&i).contains("Verbose mode"));
    }
}
