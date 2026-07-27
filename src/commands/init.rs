use anyhow::Result;

use crate::cli::InitArgs;
use crate::config::Paths;
use crate::shell;

/// Prints the integration to stdout so it can be `eval`'d:
/// `eval "$(utter init zsh)"`.
///
/// Paths are baked in as absolute values at init time rather than recomputed by
/// the shell — the hooks run on every prompt and must stay free of subprocesses.
pub fn run(args: &InitArgs, paths: &Paths) -> Result<()> {
    print!(
        "{}",
        shell::init_script(args.shell, &args.alias, &paths.shell_dir)
    );
    Ok(())
}
