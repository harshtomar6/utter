use clap::{Args, Parser, Subcommand};

use crate::config::Overrides;

#[derive(Parser, Debug)]
#[command(
    name = "utter",
    version,
    about = "Turn natural language into shell commands, delivered into your shell's input buffer",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Global flags go BEFORE the subcommand: `utter --smart gen find big files`.
    /// Everything after `gen` is treated as prompt text so a request can contain
    /// hyphens and flag-like words.
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Default)]
pub struct GlobalArgs {
    /// Model slug to use, overriding config and `--smart`.
    #[arg(long, global = true, value_name = "SLUG")]
    pub model: Option<String>,

    /// Use the stronger model configured as `smart_model`.
    #[arg(long, global = true)]
    pub smart: bool,

    /// Start a fresh conversation thread, ignoring any recent one.
    #[arg(short = 'n', long, global = true)]
    pub new: bool,

    /// Continue the most recent thread even if it is past the idle window.
    #[arg(short = 'c', long, global = true)]
    pub continue_session: bool,

    /// Delete this terminal's stored thread and exit.
    #[arg(long, global = true)]
    pub clear: bool,

    /// Print the model's reasoning to stderr alongside the command.
    #[arg(long, global = true)]
    pub explain: bool,

    /// Force the line-oriented renderer instead of the inline viewport.
    #[arg(long, global = true)]
    pub plain: bool,
}

impl GlobalArgs {
    pub fn overrides(&self) -> Overrides {
        Overrides {
            model: self.model.clone(),
            smart: self.smart,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Translate a request into a command. Writes the command to stdout and
    /// everything else to stderr. With no request, explains and fixes whatever
    /// just failed.
    Gen(GenArgs),

    /// Print the shell integration to source from your rc file.
    Init(InitArgs),

    /// Show resolved configuration and file paths.
    Config,
}

#[derive(Args, Debug)]
pub struct GenArgs {
    /// The request, as plain words. Quoting is optional.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub prompt: Vec<String>,
}

impl GenArgs {
    pub fn request(&self) -> String {
        self.prompt.join(" ").trim().to_string()
    }
}

#[derive(Args, Debug)]
pub struct InitArgs {
    #[arg(value_enum)]
    pub shell: ShellKind,

    /// Name for the shell function. Checked for collisions by install.sh.
    #[arg(long, default_value = "ask", value_name = "NAME")]
    pub alias: String,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("should parse")
    }

    #[test]
    fn gen_joins_multi_word_requests_without_quoting() {
        let cli = parse(&["utter", "gen", "find", "processes", "using", "5GB"]);
        match cli.command {
            Command::Gen(g) => assert_eq!(g.request(), "find processes using 5GB"),
            other => panic!("expected Gen, got {other:?}"),
        }
    }

    #[test]
    fn gen_accepts_flag_like_words_inside_the_request() {
        // `trailing_var_arg` + `allow_hyphen_values`: the request is prose, not
        // flags, so this must not be parsed as an unknown option.
        let cli = parse(&["utter", "gen", "what", "does", "-rf", "mean"]);
        match cli.command {
            Command::Gen(g) => assert_eq!(g.request(), "what does -rf mean"),
            other => panic!("expected Gen, got {other:?}"),
        }
    }

    #[test]
    fn gen_with_no_words_is_the_fix_last_failure_form() {
        let cli = parse(&["utter", "gen"]);
        match cli.command {
            Command::Gen(g) => assert!(g.request().is_empty()),
            other => panic!("expected Gen, got {other:?}"),
        }
    }

    #[test]
    fn global_flags_are_accepted_before_the_subcommand() {
        let cli = parse(&["utter", "--smart", "gen", "hello"]);
        assert!(cli.global.smart);
        assert!(cli.global.overrides().smart);
    }

    #[test]
    fn model_override_wins_over_smart() {
        let cli = parse(&["utter", "--model", "a/b", "--smart", "gen", "x"]);
        let o = cli.global.overrides();
        assert_eq!(o.model.as_deref(), Some("a/b"));
        assert!(o.smart);
    }

    #[test]
    fn init_defaults_the_alias_to_ask() {
        let cli = parse(&["utter", "init", "zsh"]);
        match cli.command {
            Command::Init(i) => {
                assert_eq!(i.shell, ShellKind::Zsh);
                assert_eq!(i.alias, "ask");
            }
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn init_accepts_an_alias_override() {
        let cli = parse(&["utter", "init", "fish", "--alias", "ut"]);
        match cli.command {
            Command::Init(i) => {
                assert_eq!(i.shell, ShellKind::Fish);
                assert_eq!(i.alias, "ut");
            }
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn init_rejects_an_unsupported_shell() {
        assert!(Cli::try_parse_from(["utter", "init", "csh"]).is_err());
    }

    #[test]
    fn a_missing_subcommand_is_an_error_not_a_panic() {
        assert!(Cli::try_parse_from(["utter"]).is_err());
    }

    #[test]
    fn cli_definition_is_internally_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
