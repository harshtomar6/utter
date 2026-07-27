#!/bin/sh
# utter installer.
#
#   curl -fsSL https://raw.githubusercontent.com/harshtomar6/utter/main/install.sh | sh
#
# Two jobs: put the binary somewhere on PATH, and wire up the shell integration —
# without which `utter` can print a command but never place one in your input
# buffer, which is the entire point of the tool.
#
# POSIX sh on purpose. This runs before we know anything about the machine, and
# `sh` on a Debian container is dash, not bash.
set -eu

REPO="harshtomar6/utter"
BIN="utter"
INSTALL_DIR="${UTTER_INSTALL_DIR:-$HOME/.local/bin}"
ALIAS=""
NO_MODIFY_RC=0
NO_KEY=0
# `aa` is Apple Archive, a real binary shipped with macOS — never offer it.
FALLBACK_ALIASES="ask ut utt"

usage() {
    cat <<EOF
utter installer

  --alias <name>     shell function name (default: first free of: $FALLBACK_ALIASES)
  --dir <path>       install directory (default: $INSTALL_DIR)
  --no-modify-rc     install the binary only; print the rc line instead of adding it
  --no-key           do not prompt for an API key; just print how to set one
  -h, --help         this message
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --alias) ALIAS="${2:-}"; shift 2 ;;
        --alias=*) ALIAS="${1#*=}"; shift ;;
        --dir) INSTALL_DIR="${2:-}"; shift 2 ;;
        --dir=*) INSTALL_DIR="${1#*=}"; shift ;;
        --no-modify-rc) NO_MODIFY_RC=1; shift ;;
        --no-key) NO_KEY=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

say()  { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- platform ---

detect_target() {
    kernel=$(uname -s)
    machine=$(uname -m)
    case "$kernel" in
        Darwin) os="apple-darwin" ;;
        Linux)  os="unknown-linux-gnu" ;;
        *) die "unsupported OS: $kernel (build from source: cargo install --git https://github.com/$REPO)" ;;
    esac
    case "$machine" in
        arm64|aarch64) arch="aarch64" ;;
        x86_64|amd64)  arch="x86_64" ;;
        *) die "unsupported architecture: $machine" ;;
    esac
    printf '%s-%s' "$arch" "$os"
}

# ------------------------------------------------------------------ binary ---

download() {
    target="$1"
    dest="$2"
    tag="${UTTER_VERSION:-latest}"
    if [ "$tag" = "latest" ]; then
        url="https://github.com/$REPO/releases/latest/download/${BIN}-${target}.tar.xz"
    else
        url="https://github.com/$REPO/releases/download/${tag}/${BIN}-${target}.tar.xz"
    fi

    tmp=$(mktemp -d)
    # `trap` inside a function still registers globally, which is what we want:
    # the temp dir must go away whatever happens after this point.
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "downloading $url"
    if need curl; then
        curl -fsSL "$url" -o "$tmp/utter.tar.xz" || die "download failed — is there a release for $target yet?"
    elif need wget; then
        wget -qO "$tmp/utter.tar.xz" "$url" || die "download failed — is there a release for $target yet?"
    else
        die "need curl or wget"
    fi

    # `-xf`, not `-xzf`: cargo-dist ships .tar.xz, and both BSD and GNU tar
    # auto-detect the compression from the file itself. The binary sits one level
    # down, in a `utter-<target>/` directory, which the `find` below handles.
    tar -xf "$tmp/utter.tar.xz" -C "$tmp" \
        || die "could not unpack the archive (does your tar support xz?)"
    found=$(find "$tmp" -type f -name "$BIN" -perm -u+x 2>/dev/null | head -1)
    [ -n "$found" ] || die "archive did not contain a $BIN binary"

    mkdir -p "$dest"
    # Move into place via a temp name in the SAME directory, so an upgrade cannot
    # leave a half-written binary if the copy is interrupted.
    cp "$found" "$dest/.$BIN.new"
    chmod +x "$dest/.$BIN.new"
    mv "$dest/.$BIN.new" "$dest/$BIN"
    rm -rf "$tmp"
    trap - EXIT INT TERM
}

# ------------------------------------------------------------------- shell ---

detect_shell() {
    # $SHELL is the login shell, which is what gets configured — not $0, which is
    # whatever is running this script (often sh via the curl pipe).
    case "${SHELL:-}" in
        */zsh)  echo zsh ;;
        */bash) echo bash ;;
        */fish) echo fish ;;
        *)      echo "" ;;
    esac
}

rc_file_for() {
    case "$1" in
        zsh)  echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
        fish) echo "${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish" ;;
        bash)
            # macOS Terminal starts login shells, which read .bash_profile and
            # never .bashrc. Prefer whichever already exists.
            if [ -f "$HOME/.bashrc" ]; then echo "$HOME/.bashrc"
            elif [ -f "$HOME/.bash_profile" ]; then echo "$HOME/.bash_profile"
            else echo "$HOME/.bashrc"; fi ;;
        *) echo "" ;;
    esac
}

# Is this name already taken by a command, builtin, alias or function?
#
# Run under the user's actual login shell, because `ask` might be a function
# defined in their rc file — invisible to this POSIX sh process.
name_is_taken() {
    name="$1"
    shell="$2"
    case "$shell" in
        fish)
            fish -c "type -q $name" >/dev/null 2>&1 && return 0 ;;
        zsh|bash)
            "$SHELL" -i -c "command -v $name" >/dev/null 2>&1 && return 0 ;;
        *)
            command -v "$name" >/dev/null 2>&1 && return 0 ;;
    esac
    return 1
}

MARKER="# utter shell integration"

# The alias a previous run already configured, if any.
#
# Without this, re-running the installer to upgrade would see the function that
# the *last* run installed, treat it as a collision, and silently rotate `ask` to
# `ut`. An upgrade must never rename the user's command.
existing_alias() {
    rc="$1"
    [ -f "$rc" ] || return 1
    line=$(grep -F "$MARKER" -A1 "$rc" 2>/dev/null | grep -F "utter init" | head -1) || return 1
    [ -n "$line" ] || return 1
    name=$(printf '%s' "$line" | sed -n 's/.*--alias \([A-Za-z0-9_-]*\).*/\1/p')
    [ -n "$name" ] || return 1
    printf '%s' "$name"
}

choose_alias() {
    shell="$1"
    rc="$2"

    if [ -z "$ALIAS" ]; then
        if previous=$(existing_alias "$rc"); then
            echo "$previous"
            return
        fi
    fi

    if [ -n "$ALIAS" ]; then
        # Explicit request wins, but warn rather than silently shadowing.
        if name_is_taken "$ALIAS" "$shell"; then
            warn "note: '$ALIAS' already exists; the utter function will shadow it"
        fi
        echo "$ALIAS"
        return
    fi
    for candidate in $FALLBACK_ALIASES; do
        if ! name_is_taken "$candidate" "$shell"; then
            echo "$candidate"
            return
        fi
    done
    echo ""
}

# Idempotent: never append a second time, and replace the line if the install
# path or alias changed.
update_rc() {
    rc="$1"
    line="$2"
    mkdir -p "$(dirname "$rc")"
    [ -f "$rc" ] || : > "$rc"

    if grep -Fq "$MARKER" "$rc" 2>/dev/null; then
        existing=$(grep -F "$MARKER" -A1 "$rc" | tail -1)
        if [ "$existing" = "$line" ]; then
            say "already configured in $rc"
            return
        fi
        # Rewrite the managed block rather than stacking a second one.
        tmp="${rc}.utter.$$"
        grep -v -F "$MARKER" "$rc" | grep -v -F "utter init" > "$tmp" || true
        printf '%s\n%s\n' "$MARKER" "$line" >> "$tmp"
        mv "$tmp" "$rc"
        say "updated the utter block in $rc"
        return
    fi

    printf '\n%s\n%s\n' "$MARKER" "$line" >> "$rc"
    say "added the utter block to $rc"
}

# --------------------------------------------------------------- api key ---

KEYS_URL="https://openrouter.ai/keys"

key_instructions() {
    say ""
    say "utter needs an OpenRouter API key. Get one at $KEYS_URL"
    say "then either:"
    say "  export OPENROUTER_API_KEY=sk-or-v1-...        (add to $1)"
    say "  or put  api_key = \"sk-or-v1-...\"  in $2"
}

# Offer to store the key, so the user is not left to work out TOML on their own.
#
# Reads from /dev/tty rather than stdin: under `curl ... | sh` stdin is the
# script itself, and a `read` there would swallow the rest of this file.
configure_key() {
    rc="$1"
    config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/utter"
    config_file="$config_dir/config.toml"

    if [ -n "${OPENROUTER_API_KEY:-}" ]; then
        say ""
        say "OPENROUTER_API_KEY is already set in your environment — nothing to do."
        return
    fi
    if [ -f "$config_file" ] && grep -q '^[[:space:]]*api_key' "$config_file" 2>/dev/null; then
        say ""
        say "an api key is already configured in $config_file"
        return
    fi
    # Actually try to open the terminal rather than testing the device file.
    # `[ -r /dev/tty ]` can succeed in CI where the node exists but the process
    # has no controlling terminal — and a `read` there would hang the build.
    # The probe runs in a subshell: a redirection failure is reported by the
    # shell itself, so `2>/dev/null` on the bare command does not suppress it and
    # a container install prints "/dev/tty: Device not configured" before the
    # instructions. Redirecting the subshell's stderr does suppress it.
    if [ "$NO_KEY" -eq 1 ] || ! ( : < /dev/tty ) 2>/dev/null; then
        key_instructions "$rc" "$config_file"
        return
    fi

    say ""
    say "utter needs an OpenRouter API key — get one at $KEYS_URL"
    printf 'paste it here (hidden), or press Enter to skip: '

    # Never echo a secret. Restore the terminal even on Ctrl-C.
    echo_off=0
    if stty -echo </dev/tty 2>/dev/null; then
        echo_off=1
        trap 'stty echo </dev/tty 2>/dev/null; exit 130' INT TERM
    fi
    key=""
    read -r key </dev/tty || key=""
    if [ "$echo_off" -eq 1 ]; then
        stty echo </dev/tty 2>/dev/null
        trap - INT TERM
    fi
    printf '\n'

    if [ -z "$key" ]; then
        key_instructions "$rc" "$config_file"
        return
    fi

    # A quote or backslash would break out of the TOML basic string below. Real
    # OpenRouter keys contain neither, so this is a corrupted paste.
    case "$key" in
        *'"'*|*'\'*)
            warn "that key contains a quote or backslash, which is not a valid key — skipping."
            key_instructions "$rc" "$config_file"
            return ;;
    esac
    case "$key" in
        sk-or-*) ;;
        *) warn "note: OpenRouter keys usually start with 'sk-or-' — saving it anyway." ;;
    esac

    mkdir -p "$config_dir"
    # umask, not a later chmod: the file is never world-readable, not even briefly.
    (umask 077; printf 'api_key = "%s"\n' "$key" > "$config_file") \
        || die "could not write $config_file"
    say "saved to $config_file (permissions 600)"
}

# -------------------------------------------------------------------- main ---

target=$(detect_target)
say "installing utter for $target"
download "$target" "$INSTALL_DIR"
say "installed $INSTALL_DIR/$BIN"

shell=$(detect_shell)
if [ -z "$shell" ]; then
    warn ""
    warn "could not identify your shell from \$SHELL (${SHELL:-unset})."
    warn "add the matching line to your rc file by hand:"
    warn "  zsh/bash:  eval \"\$($INSTALL_DIR/$BIN init <shell>)\""
    warn "  fish:      $INSTALL_DIR/$BIN init fish | source"
    exit 0
fi

rc=$(rc_file_for "$shell")
name=$(choose_alias "$shell" "$rc")
if [ -z "$name" ]; then
    warn ""
    warn "every candidate name is already taken ($FALLBACK_ALIASES)."
    warn "pick your own and re-run:  ... | sh -s -- --alias <name>"
    exit 1
fi
say "shell: $shell, function name: $name"

if [ "$shell" = "fish" ]; then
    line="$INSTALL_DIR/$BIN init fish --alias $name | source"
else
    line="eval \"\$($INSTALL_DIR/$BIN init $shell --alias $name)\""
fi

if [ "$NO_MODIFY_RC" -eq 1 ]; then
    say ""
    say "add this to your rc file:"
    say "  $line"
    exit 0
fi

update_rc "$rc" "$line"

# PATH advice only if it is actually needed.
case ":${PATH}:" in
    *":$INSTALL_DIR:"*) ;;
    *) warn ""
       warn "note: $INSTALL_DIR is not on your PATH."
       warn "the integration uses an absolute path so it works regardless,"
       warn "but add it if you want to run '$BIN' directly." ;;
esac

say ""
say "done. start a new shell, or:"
if [ "$shell" = "fish" ]; then
    say "  source $rc"
else
    say "  . $rc"
fi
if [ "$shell" = "bash" ]; then
    say ""
    say "bash note: '$name <request>' prints the command. To place it in your input"
    say "buffer, type the request on the command line and press Ctrl-G — bash has no"
    say "equivalent of zsh's 'print -z'."
fi
configure_key "$rc"

say ""
say "then try:  $name find the largest files in this directory"
