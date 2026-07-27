use std::time::Duration;

use anyhow::Result;

use crate::config::{Config, Paths, DEFAULT_BASE_URL, DEFAULT_MODEL, ENV_API_KEY};
use crate::context::ShellContext;
use crate::session::{self, SessionKey};

/// Everything here goes to stdout: `utter config` is a report, not a command, so
/// there is no captured value to protect.
pub fn run(cfg: &Config, paths: &Paths) -> Result<()> {
    let ctx = ShellContext::probe();

    println!("resolved config");
    println!("  api key            {}", cfg.redacted_api_key());
    println!("  base url           {}", cfg.base_url);
    println!("  model              {}", cfg.model);
    println!("  max tokens         {}", cfg.max_tokens);
    println!("  temperature        {}", cfg.temperature);
    println!("  session idle       {}s", cfg.session_idle_secs);
    println!("  history budget     {} tokens", cfg.history_token_budget);
    println!("  output cap         {} bytes", cfg.captured_output_limit);
    if let Some(referer) = &cfg.referer {
        println!("  http-referer       {referer}");
    }
    if let Some(title) = &cfg.title {
        println!("  x-title            {title}");
    }

    println!("\npaths");
    println!(
        "  config file        {}{}",
        paths.config_file.display(),
        if paths.config_file.exists() {
            ""
        } else {
            "  (does not exist)"
        }
    );
    println!("  sessions           {}", paths.sessions_dir.display());
    println!("  shell state        {}", paths.shell_dir.display());

    println!("\ndetected environment");
    for line in ctx.render().lines() {
        println!("  {line}");
    }

    // "Are the hooks working?" is the first question anyone asks when the bare
    // invocation does nothing, so answer it here rather than making them guess.
    println!("\nshell integration");
    let key = SessionKey::resolve();
    if key.is_detached() {
        println!(
            "  status             not loaded (no {} in environment)",
            session::ENV_SESSION_ID
        );
        println!("  fix                add `eval \"$(utter init <shell>)\"` to your rc file");
    } else {
        println!("  status             loaded");
        println!("  shell session      {}", key.as_str());

        let idle = Duration::from_secs(cfg.session_idle_secs);
        match session::last_command(paths, &key, idle) {
            Ok(Some(state)) => {
                println!("  last command       {}", state.command);
                println!(
                    "  last exit code     {}{}",
                    state.exit_code,
                    if state.failed() {
                        "  (bare `ask` will offer a fix)"
                    } else {
                        ""
                    }
                );
            }
            Ok(None) => println!("  last command       none recorded yet"),
            Err(e) => println!("  last command       unreadable: {e:#}"),
        }

        let path = session::file_path(paths, &key);
        match session::load(paths, &key, &cfg.model, idle, Default::default()) {
            Ok((s, _)) if path.exists() => {
                println!("  conversation       {} turn(s) stored", s.turn_count())
            }
            _ => println!("  conversation       none stored"),
        }
    }

    if !cfg.has_api_key() {
        println!(
            "\nno api key set. get one at https://openrouter.ai/keys, then either:\n  \
             export {ENV_API_KEY}=sk-or-v1-...\n  \
             or add `api_key = \"sk-or-v1-...\"` to {}",
            paths.config_file.display()
        );
    }
    if cfg.base_url != DEFAULT_BASE_URL {
        println!("\nusing a non-default gateway (default is {DEFAULT_BASE_URL})");
    }
    if cfg.model != DEFAULT_MODEL {
        println!("model overridden (default is {DEFAULT_MODEL})");
    }

    Ok(())
}
