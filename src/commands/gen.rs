use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::{bail, Result};

use crate::cli::{GenArgs, GlobalArgs};
use crate::config::{Config, Paths};
use crate::context::ShellContext;
use crate::conversation;
use crate::error::UtterError;
use crate::llm::{self, tools, Client, ModelOutcome};
use crate::output;
use crate::piped;
use crate::prompt::{self, Failure, PromptInput};
use crate::scanner;
use crate::session::{self, Continuity, LoadOptions, SessionKey, ShellState};

/// One retry. The retry exists so a malformed tool call gets a second chance with
/// the parse error fed back; more than that just makes a human wait.
const MAX_ATTEMPTS: u32 = 2;

/// Stands in for the tool result that Phase 2 will fill with real captured output.
///
/// Something has to occupy this slot: an assistant `tool_calls` entry with no
/// matching `tool` message makes the *next* request 400. It is also honest context
/// for the following turn — the model proposed a command and never learned what
/// happened.
const NOT_EXECUTED: &str = "The command was placed in the user's shell buffer for them to review. \
     It was not executed and no output is available.";

pub async fn run(args: &GenArgs, global: &GlobalArgs, cfg: &Config, paths: &Paths) -> Result<()> {
    let key = SessionKey::resolve();
    let idle = Duration::from_secs(cfg.session_idle_secs);

    // Piping is a deliberate act, so it takes precedence over the bare-invocation
    // reading: `cmd | ask` means "explain this", not "fix the last failure".
    let piped = piped::read(cfg.captured_output_limit);

    let raw_request = args.request();
    let (request, failure) = if !raw_request.is_empty() {
        (raw_request, None)
    } else if piped.is_some() {
        ("Explain what this output means.".to_string(), None)
    } else {
        // Bare invocation with nothing piped: fix whatever just failed.
        let state = resolve_failure(paths, &key, idle, global)?;
        (describe_failure(&state), Some(state))
    };

    let ctx = ShellContext::probe();
    let system = prompt::build(&PromptInput {
        ctx: &ctx,
        piped: piped.as_deref(),
        failure: failure.as_ref().map(|f| Failure {
            command: &f.command,
            exit_code: f.exit_code,
            cwd: f.cwd.as_deref(),
            parse_error: f.parse_error,
        }),
        explain: global.explain,
    });

    let (mut stored, continuity) = session::load(
        paths,
        &key,
        &cfg.model,
        idle,
        LoadOptions {
            force_new: global.new,
            force_continue: global.continue_session,
        },
    )?;

    let mut renderer = output::pick(global.plain);

    // Silent for the two ordinary cases. A dropped history is worth a word; a
    // normal continuation is not — the same asymmetric-friction reasoning as the
    // risk banners.
    match continuity {
        Continuity::ExpiredAndRestarted => {
            renderer.note("previous thread was idle too long — starting fresh")?
        }
        Continuity::UnreadableAndRestarted => {
            renderer.note("stored thread was unreadable — starting fresh")?
        }
        Continuity::New | Continuity::Resumed | Continuity::Restarted => {}
    }

    // The system prompt is rebuilt here and never came from disk.
    let mut messages = stored.messages.clone();
    conversation::refresh_system(&mut messages, system);
    conversation::push_user(&mut messages, &request);
    conversation::trim_to_budget(&mut messages, cfg.history_token_budget);

    let client = Client::new(cfg)?;

    for attempt in 1..=MAX_ATTEMPTS {
        conversation::validate(&messages)?;

        renderer.start("thinking")?;
        let result = await_with_spinner(
            client.chat(
                &cfg.model,
                &messages,
                tools::definitions(),
                cfg.max_tokens,
                cfg.temperature,
            ),
            renderer.as_mut(),
        )
        .await;
        // Always clear the spinner, including on the error path, before anything
        // else touches stderr.
        renderer.stop()?;
        let response = result?;

        match llm::classify(&response) {
            ModelOutcome::Command {
                call,
                text,
                proposal,
            } => {
                if let Some(narration) = text.as_deref().filter(|_| global.explain) {
                    renderer.note(narration)?;
                }

                let scan = scanner::scan(&proposal.command);
                let effective = scanner::effective_risk(proposal.risk, scan.risk);
                renderer.proposal(&proposal, &scan, effective)?;

                let call_id = call.id.clone();
                conversation::push_tool_call(&mut messages, text, call);
                conversation::push_tool_result(&mut messages, call_id, NOT_EXECUTED);
                persist(paths, &key, &mut stored, &messages, &mut renderer);

                // The one and only write to stdout.
                emit(&proposal.command)?;
                return Ok(());
            }

            // A valid answer, not a failure: the model judged that no shell
            // command serves the request.
            ModelOutcome::Text(body) => {
                renderer.text(&body)?;
                conversation::push_assistant_text(&mut messages, body);
                persist(paths, &key, &mut stored, &messages, &mut renderer);
                return Ok(());
            }

            ModelOutcome::MalformedArgs { call, error } => {
                if attempt == MAX_ATTEMPTS {
                    return Err(UtterError::NoCommand { attempts: attempt }.into());
                }
                // Record the turn and hand the parse error back as a tool result.
                // Skipping the assistant message would orphan the tool result and
                // 400 the next request.
                let call_id = call.id.clone();
                conversation::push_tool_call(&mut messages, None, call);
                conversation::push_tool_result(
                    &mut messages,
                    call_id,
                    format!("Error: {error}. Call run_command again with valid JSON arguments."),
                );
                renderer.note("model returned malformed arguments — retrying")?;
            }

            ModelOutcome::UnknownTool { call } => {
                if attempt == MAX_ATTEMPTS {
                    return Err(UtterError::NoCommand { attempts: attempt }.into());
                }
                let call_id = call.id.clone();
                let name = call.function.name.clone();
                conversation::push_tool_call(&mut messages, None, call);
                conversation::push_tool_result(
                    &mut messages,
                    call_id,
                    format!(
                        "Error: no tool named `{name}` exists. The only tool is `run_command`."
                    ),
                );
                renderer.note("model called an unknown tool — retrying")?;
            }

            // Nothing to act on and nothing to retry with. Fail rather than hang.
            ModelOutcome::Empty => return Err(UtterError::NoCommand { attempts: attempt }.into()),
        }
    }

    Err(UtterError::NoCommand {
        attempts: MAX_ATTEMPTS,
    }
    .into())
}

/// Drives the renderer's animation while the request is in flight.
///
/// The tick is pushed from here rather than pulled by a task the renderer owns:
/// the runtime is `current_thread`, and a `select!` keeps the renderer synchronous
/// and free of `Arc<Mutex<_>>`. A failed redraw is swallowed — a broken spinner
/// must never take down a request that is otherwise fine.
async fn await_with_spinner<F, T>(future: F, renderer: &mut dyn output::Renderer) -> T
where
    F: std::future::Future<Output = T>,
{
    const FRAME: Duration = Duration::from_millis(80);

    let mut future = std::pin::pin!(future);
    let mut ticker = tokio::time::interval(FRAME);
    // The first tick completes immediately; skip it so the spinner does not
    // double-draw the frame `start` already painted.
    ticker.tick().await;

    loop {
        tokio::select! {
            output = &mut future => return output,
            _ = ticker.tick() => {
                let _ = renderer.tick();
            }
        }
    }
}

/// Finds the failure a bare invocation is meant to fix, or explains why there
/// isn't one.
fn resolve_failure(
    paths: &Paths,
    key: &SessionKey,
    idle: Duration,
    global: &GlobalArgs,
) -> Result<ShellState> {
    if key.is_detached() {
        bail!(
            "no request given, and no shell integration is loaded.\n  \
             run `utter init <zsh|bash|fish>` from your rc file to enable \
             fixing the last failure.\n  \
             otherwise: {} <what you want>",
            if global.plain { "utter gen" } else { "ask" }
        );
    }

    match session::last_command(paths, key, idle)? {
        Some(state) if state.failed() => Ok(state),
        Some(state) => bail!(
            "the last command succeeded, so there is nothing to fix:\n  {}\n  \
             describe what you want instead: ask <request>",
            state.command
        ),
        None => bail!(
            "no recent command recorded for this shell.\n  \
             if you just added the integration, start a new shell or re-source your rc file.\n  \
             otherwise: ask <what you want>"
        ),
    }
}

/// A self-contained restatement of the failure.
///
/// The system prompt also carries this, but the *user* message is what gets
/// persisted — a stored turn reading "fix it" would be useless context two turns
/// later.
fn describe_failure(state: &ShellState) -> String {
    let mut request = if state.parse_error {
        format!(
            "The shell could not parse this line, so it never ran: `{}`",
            state.command
        )
    } else {
        format!(
            "The command `{}` just failed with exit code {}.",
            state.command, state.exit_code
        )
    };
    if let Some(cwd) = &state.cwd {
        // "It ran in ..." would contradict the sentence above for a line the
        // shell refused to execute.
        if state.parse_error {
            request.push_str(&format!(" It was typed in {cwd}."));
        } else {
            request.push_str(&format!(" It ran in {cwd}."));
        }
    }
    request.push_str(" Explain the cause in one line and give me the corrected command.");
    request
}

/// A failed write must not fail the request: the command is already correct and
/// the user is waiting for it. Losing continuity is the lesser harm, so this
/// reports and moves on.
fn persist(
    paths: &Paths,
    key: &SessionKey,
    stored: &mut session::Session,
    messages: &[crate::llm::Message],
    renderer: &mut Box<dyn output::Renderer>,
) {
    let history = conversation::history(messages);
    if let Err(e) = session::save(paths, key, stored, history) {
        let _ = renderer.note(&format!("could not save session: {e:#}"));
    }
}

/// Writes the command to stdout, and nothing else ever does.
///
/// When stdout is a TTY the shell function is not capturing us — either the user
/// ran `utter gen` directly or the integration is not sourced. The command still
/// prints, so the tool stays pipeable; the hint goes to stderr where it cannot
/// contaminate the captured value.
fn emit(command: &str) -> Result<()> {
    let mut out = std::io::stdout();
    writeln!(out, "{command}")?;
    out.flush()?;

    if out.is_terminal() {
        let mut err = std::io::stderr();
        writeln!(
            err,
            "\x1b[2m(not captured by a shell function — run `utter init <shell>` for buffer insertion)\x1b[0m"
        )?;
        err.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(command: &str, exit_code: i32, cwd: Option<&str>) -> ShellState {
        ShellState {
            command: command.to_string(),
            exit_code,
            cwd: cwd.map(str::to_string),
            parse_error: false,
        }
    }

    #[test]
    fn the_persisted_request_restates_the_failure_in_full() {
        // Must stand on its own: this is what a later turn reads back.
        let request = describe_failure(&state("tar -xf a.tar.gz", 1, Some("/tmp")));
        assert!(request.contains("tar -xf a.tar.gz"));
        assert!(request.contains("exit code 1"));
        assert!(request.contains("/tmp"));
        assert!(request.contains("corrected command"));
    }

    #[test]
    fn a_parse_error_never_claims_the_command_ran() {
        let mut st = state("find . -exec stat {} ; | sort", 1, Some("/tmp"));
        st.parse_error = true;
        let request = describe_failure(&st);
        assert!(request.contains("could not parse"));
        assert!(request.contains("never ran"));
        assert!(!request.contains("It ran in"), "{request}");
        assert!(request.contains("It was typed in /tmp"));
    }

    #[test]
    fn a_failure_without_a_cwd_still_describes_cleanly() {
        let request = describe_failure(&state("git psuh", 1, None));
        assert!(request.contains("git psuh"));
        assert!(!request.contains("It ran in"));
    }

    #[test]
    fn the_synthetic_tool_result_does_not_claim_output_exists() {
        // The model must not be led to believe it saw the command run.
        assert!(NOT_EXECUTED.contains("not executed"));
        assert!(NOT_EXECUTED.contains("no output"));
    }
}
