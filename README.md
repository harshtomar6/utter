# utter

Natural language to shell commands, delivered **into your shell's input buffer**.

```
$ ask find processes using more than 5GB of ram
$ ps axo pid,rss,comm | sort -nrk2 | awk '$2>5242880'    ← appears at your prompt, cursor at the end
```

Nothing runs. The command lands where you would have typed it, and you edit it or
press Enter yourself.

The competition here is `Ctrl-R` and a browser tab, not a coding agent. `utter` is
not an app you enter — it is a layer your shell loads, so it never takes over the
screen and never moves you away from the prompt you were already at.

## Fixing what just broke

Run it with no arguments after something fails:

```
$ tar -xf archive.tar.gz
tar: Error opening archive: Unrecognized archive format
$ ask
caution: The archive is gzip-compressed, so -x alone fails; -z adds decompression.
$ tar -xzf archive.tar.gz    ← in your buffer, ready to run
```

Shell hooks record the last command and its exit code, so a bare `ask` means
"explain and fix that." This is the thing a launched-app assistant structurally
cannot do.

## When the output needs explaining

Pipe it back:

```
$ ps axo pid,rss,comm | sort -nrk2 | head | ask why is my ram full
firefox is using ~7.6 GB of resident memory, by far the largest consumer.
```

The answer goes to stderr, so nothing lands in your input buffer. Drop the
question and it just explains what it sees:

```
$ kubectl describe pod api-7f9 | ask
```

You run the command, you look at the result, and you decide what the model sees.
Nothing is captured behind your back and nothing runs on the model's behalf.

Piped text is untrusted — a log or an HTTP response may contain text shaped like
an instruction. It is fenced and labelled as data, and the model is told to
describe such text rather than act on it. That is a mitigation, not a guarantee,
which is why a human stays between the model and execution.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/harshtomar6/utter/main/install.sh | sh
```

It downloads the binary, works out your shell, adds one line to your rc file, and
prompts for an API key:

```
installing utter for aarch64-apple-darwin
installed ~/.local/bin/utter
shell: zsh, function name: ask
added the utter block to ~/.zshrc

utter needs an OpenRouter API key — get one at https://openrouter.ai/keys
paste it here (hidden), or press Enter to skip: ****
saved to ~/.config/utter/config.toml (permissions 600)

done. start a new shell, or:  . ~/.zshrc
then try:  ask find the largest files in this directory
```

Input is not echoed, and the file is created with mode 600 from the start. Press
Enter to skip and it prints how to set the key yourself. It never prompts when
`OPENROUTER_API_KEY` is already set, when a key is already in the config file, or
when there is no terminal to read from — so piping into a container build cannot
hang.

The whole thing is idempotent: re-running upgrades in place, keeps the function
name you already chose, and leaves an existing key alone.

```
--alias <name>     function name (default: first free of ask, ut, utt)
--dir <path>       install location (default: ~/.local/bin)
--no-modify-rc     install the binary only and print the rc line
--no-key           never prompt for a key
```

Manual setup, if you would rather not pipe a script into `sh`:

```sh
cargo install --git https://github.com/harshtomar6/utter
echo 'eval "$(utter init zsh)"' >> ~/.zshrc          # zsh
echo 'eval "$(utter init bash)"' >> ~/.bashrc        # bash
echo 'utter init fish | source' >> ~/.config/fish/config.fish   # fish
```

## Shell support

| Shell | How the command reaches you | Mechanism |
|---|---|---|
| zsh  | `ask <request>` → command at your next prompt | `print -z` |
| fish | `ask <request>` → command on the command line | `commandline -r` |
| bash | type the request, press **Ctrl-G** | `bind -x` + `READLINE_LINE` |

**bash works differently, and that is not a bug.** bash has no equivalent of
`print -z`; the only way to write the input buffer is `bind -x`, which needs a
keypress. So in bash you type your request on the command line and hit Ctrl-G to
swap it for the command. `ask <request>` still works there, but it prints the
command rather than inserting it.

## Commands

```
utter gen <request...>   translate a request; command → stdout, everything else → stderr
utter gen                fix whatever just failed
utter init <shell>       print the shell integration
utter config             show resolved settings, paths, and integration status
```

Flags: `--model <slug>`, `--smart`, `--explain`, `--plain`, `-n/--new`,
`-c/--continue`, `--clear`.

`utter config` reports whether the hooks are loaded and what the last recorded
command was — start there if a bare `ask` does nothing.

## Configuration

`OPENROUTER_API_KEY`, else `api_key` in `~/.config/utter/config.toml`:

```toml
model = "anthropic/claude-haiku-4.5"
smart_model = "anthropic/claude-sonnet-5"        # used by --smart
base_url = "https://openrouter.ai/api/v1"
max_tokens = 1024
temperature = 0.2
session_idle_secs = 1800                          # 0 disables conversation threading
history_token_budget = 8000
```

`base_url` accepts any OpenAI-compatible endpoint, so you can point it at another
gateway or a local model — see the privacy note below.

State lives in `~/.local/state/utter/` (sessions and last-command records), on
Linux **and** macOS. `directories` would put this under
`~/Library/Application Support` on macOS, but the shell hooks and the installer
need one documented path per file on both platforms.

## Safety

**Nothing auto-executes.** You pressing Enter at your own prompt is the approval
step, and it is a stronger one than any confirmation dialog.

Every command is also checked by a local scanner covering `rm -rf` at the root or
your home directory, `dd` to a block device, `mkfs`, fork bombs, `curl | sh`,
recursive `chmod`/`chown` over system paths, force-push, redirects over system
files, and more. The displayed risk is `max(model's own rating, scanner's rating)`
— the scanner can raise a risk the model understated, never lower one.

Risk levels are asymmetric on purpose: `safe` prints one dim line, `danger` prints
a loud banner with the destructive fragment highlighted. Approval fatigue is the
real failure mode for tools like this, so routine commands stay quiet in order
that warnings keep meaning something. `sudo apt update` is `caution`, not `danger`.

**The scanner is a UI affordance, not a security boundary.** It is regex over an
unparsed string, and the shell defeats it trivially:

```sh
X=rm; $X -rf ~                     # the verb is in a variable
RF="-rf"; rm $RF /                 # the flags are in a variable
$(echo cm0gLXJmIC8K | base64 -d)   # the whole command is encoded
```

Do not build anything that treats a `safe` verdict as authoritative.

**Prompt injection is live, not theoretical.** In Phase 2, when captured command
output feeds back into the model, that output — logs, HTTP responses, file
contents — can carry adversarial instructions. That is the main argument for
keeping a human between the model and execution, and why v1 has no auto-run mode.

## Privacy

Your request, and the machine context below, are sent to OpenRouter and on to a
model provider:

- OS and architecture, coreutils flavour (BSD/GNU), `$SHELL`
- your current working directory, as a path
- which of a fixed list of tools are on your `PATH`
- for a bare `ask`: the failed command text, its exit code, and its directory

Command **output** is sent only when you pipe it in yourself (`cmd | ask ...`).
The shell hooks cannot see output and never capture it; nothing leaves the
machine unless you put it there.

Working directory paths and command text routinely contain project, client and
hostname information. If that matters to you:

- OpenRouter has a data-collection routing preference in your account settings
  that controls whether providers may retain or train on your prompts.
- Point `base_url` at a local OpenAI-compatible server (llama.cpp, Ollama, vLLM)
  and nothing leaves the machine.

No telemetry. `utter` talks to exactly one host: whatever `base_url` names.

## Development

```sh
cargo test                              # unit tests
cargo clippy --all-targets -- -D warnings
cargo fmt
UTTER_BASE_URL=http://127.0.0.1:8080/v1 cargo run -- gen list large files
```

`--plain` forces the line renderer, which is the scriptable path and the one to
use when iterating on the system prompt.

### Release

Tag and push; `.github/workflows/release.yml` builds all four targets through
cargo-dist and publishes to GitHub Releases.

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The workflow is hand-authored. `dist generate --mode=ci` exits 0 without writing
anything on cargo-dist 0.32, and the interactive `dist init` wizard cannot be
driven headlessly, so the job wiring is ours. The build steps still call
`dist build`, so artifact names, checksums and the generated installer are exactly
what `dist plan` reports.

Because cargo-dist expects to own `release.yml`, the config sets
`allow-dirty = ["ci"]`; without it `dist plan` fails in CI on a freshness check
against the file it would have generated.

### A note on the spinner

The plan was ratatui with `Viewport::Inline`. It cannot be used here.
`Viewport::Inline` needs the cursor row, so it calls
`Backend::get_cursor_position()`, and `CrosstermBackend` implements that via
`crossterm::cursor::position()` — which writes `\x1b[6n` to a hardcoded
`io::stdout()` regardless of the writer the backend was built with
(`crossterm-0.29.0/src/cursor/sys/unix.rs`). The escape then lands in the captured
command and gets inserted into your buffer. stdout carrying the command and
nothing else is the invariant the tool rests on, so the spinner is written
directly to stderr instead and ratatui is not a dependency.

## Status

v1 is the translator and the failure-fixer. Not built, by design: no agent loop,
no sandboxing, no MCP, no telemetry, no auto-approve.

Phase 2 is the captured multi-step loop behind `needs_output` — permission cards,
tool results fed back, a `finish` tool. The message-building and tool-dispatch
layers are already separate from the output path so it drops in cleanly.

## License

MIT
