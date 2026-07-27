use crate::risk::Risk;

/// One detection rule.
///
/// A rule fires only when **every** regex in `all_of` matches somewhere in the
/// command. That split exists because the `regex` crate has no lookaround, and
/// because "an `rm` that is recursive AND aimed at a critical path" written as a
/// single expression is unreadable and untestable. Flag order, interleaved
/// arguments and combined short flags all fall out for free.
pub struct Pattern {
    pub rule: &'static str,
    pub risk: Risk,
    pub note: &'static str,
    pub all_of: &'static [&'static str],
    /// Index into `all_of` whose match supplies the highlight span — point at the
    /// fragment a human needs to see, which is usually the target, not the verb.
    pub span_from: usize,
}

/// `rm` in **command position** — at the start, after a separator, or after
/// whitespace — and not as part of a flag.
///
/// A bare `\brm\b` matches the `--rm` in `docker run --rm -it ubuntu`, and
/// `--force-rm` in `docker build --force-rm`. Both are among the most common
/// commands a developer types; flagging them trains the user to ignore the label.
const RM_COMMAND: &str = r"(?:^|[;&|(]\s*|\s)rm(?:\s|$)";

/// Destructive verbs, also constrained to command position so an argument that
/// merely contains the word does not escalate the whole command.
const DESTRUCTIVE_VERB: &str = r"(?:^|[;&|(]\s*|\s)(?:rm|dd|mkfs|chmod|chown|shutdown|reboot|halt|poweroff|killall|dscl|diskutil|fdisk|parted)(?:\s|$)";

/// Matches the recursive flag in any short-flag arrangement: `-r`, `-rf`, `-fr`,
/// `-Rf`, `--recursive`.
///
/// Note this also matches the literal flag `--rm`, which is why every rule using
/// it pairs it with `RM_COMMAND` rather than a bare `rm` match.
const RECURSIVE_FLAG: &str = r"(?:\s-{1,2}[a-zA-Z]*[rR][a-zA-Z]*\b|\s--recursive\b)";
const FORCE_FLAG: &str = r"(?:\s-{1,2}[a-zA-Z]*f[a-zA-Z]*\b|\s--force\b)";

/// Root, `$HOME` and their glob forms — the targets that turn a delete into an
/// unrecoverable one. Note `~/Documents` deliberately does NOT match: a named
/// subdirectory is a normal recursive delete, not a home-directory wipe.
const CRITICAL_TARGET: &str = r"\s(?:/|~|~/|\$HOME/?|\$\{HOME\}/?|/\*|~/\*|\$HOME/\*)(?:\s|$)";

/// Directories owned by the OS or package manager.
const SYSTEM_PATH: &str = r"\s/(?:usr|etc|bin|sbin|var|lib|lib64|boot|opt|srv|System|Library|Applications|private|Volumes)(?:/|\s|$)";

const CRITICAL_OR_SYSTEM: &str =
    r"\s(?:/|~|~/|\$HOME/?|/usr|/etc|/bin|/sbin|/var|/lib|/boot|/System|/Library)(?:/|\s|$)";

/// Block devices. Deliberately excludes `/dev/null`, `/dev/zero` and
/// `/dev/urandom`, which appear in entirely ordinary commands.
const BLOCK_DEVICE: &str = r"/dev/(?:disk\d|rdisk\d|sd[a-z]|nvme\d|hd[a-z]|mmcblk\d)";
const REDIRECT_TO_BLOCK_DEVICE: &str =
    r">\s*/dev/(?:disk\d|rdisk\d|sd[a-z]|nvme\d|hd[a-z]|mmcblk\d)";

pub static PATTERNS: &[Pattern] = &[
    // ---- deletion -------------------------------------------------------
    Pattern {
        rule: "rm-recursive-critical-target",
        risk: Risk::Danger,
        note: "recursive delete aimed at the filesystem root or your home directory",
        all_of: &[RM_COMMAND, RECURSIVE_FLAG, CRITICAL_TARGET],
        span_from: 2,
    },
    Pattern {
        rule: "rm-recursive-system-path",
        risk: Risk::Danger,
        note: "recursive delete inside an OS-owned directory",
        all_of: &[RM_COMMAND, RECURSIVE_FLAG, SYSTEM_PATH],
        span_from: 2,
    },
    Pattern {
        rule: "rm-recursive",
        risk: Risk::Caution,
        note: "deletes a directory tree",
        all_of: &[RM_COMMAND, RECURSIVE_FLAG],
        span_from: 1,
    },
    Pattern {
        rule: "rm-force",
        risk: Risk::Caution,
        note: "deletes without prompting",
        all_of: &[RM_COMMAND, FORCE_FLAG],
        span_from: 1,
    },
    Pattern {
        rule: "find-delete",
        risk: Risk::Caution,
        note: "deletes every file the search matches",
        all_of: &[r"\bfind\b", r"(?:-delete\b|-exec\s+rm\b)"],
        span_from: 1,
    },
    // ---- whole-disk operations -----------------------------------------
    Pattern {
        rule: "dd-to-block-device",
        risk: Risk::Danger,
        note: "writes directly to a raw disk device, destroying its contents",
        all_of: &[r"\bdd\b", r"\bof=", BLOCK_DEVICE],
        span_from: 2,
    },
    Pattern {
        rule: "dd-write",
        risk: Risk::Caution,
        note: "dd overwrites its output target in place",
        all_of: &[r"\bdd\b", r"\bof="],
        span_from: 1,
    },
    Pattern {
        rule: "mkfs",
        risk: Risk::Danger,
        note: "formats a filesystem, erasing everything on the target",
        all_of: &[r"\bmkfs(?:\.[a-z0-9]+)?\b"],
        span_from: 0,
    },
    Pattern {
        rule: "disk-partition-tool",
        risk: Risk::Danger,
        note: "repartitions or erases a disk",
        all_of: &[r"\b(?:diskutil\s+(?:erase\w*|partitionDisk|reformat)|fdisk|parted|sgdisk)\b"],
        span_from: 0,
    },
    Pattern {
        rule: "redirect-to-block-device",
        risk: Risk::Danger,
        note: "redirects output onto a raw disk device",
        all_of: &[REDIRECT_TO_BLOCK_DEVICE],
        span_from: 0,
    },
    // ---- overwriting system state --------------------------------------
    Pattern {
        rule: "redirect-over-system-path",
        risk: Risk::Danger,
        note: "overwrites a file in an OS-owned directory",
        all_of: &[r">\s*/(?:etc|usr|bin|sbin|boot|lib|System|Library)/"],
        span_from: 0,
    },
    Pattern {
        rule: "chmod-recursive-critical",
        risk: Risk::Danger,
        note: "rewrites permissions across the root or an OS-owned directory",
        all_of: &[r"\bchmod\b", RECURSIVE_FLAG, CRITICAL_OR_SYSTEM],
        span_from: 2,
    },
    Pattern {
        rule: "chmod-recursive",
        risk: Risk::Caution,
        note: "rewrites permissions across a whole tree",
        all_of: &[r"\bchmod\b", RECURSIVE_FLAG],
        span_from: 1,
    },
    Pattern {
        rule: "chmod-world-writable",
        risk: Risk::Caution,
        note: "makes the target writable by every user on the machine",
        all_of: &[r"\bchmod\b", r"\s0?777\b"],
        span_from: 1,
    },
    Pattern {
        rule: "chown-recursive-critical",
        risk: Risk::Danger,
        note: "rewrites ownership across the root or an OS-owned directory",
        all_of: &[r"\bchown\b", RECURSIVE_FLAG, CRITICAL_OR_SYSTEM],
        span_from: 2,
    },
    // ---- remote code execution -----------------------------------------
    Pattern {
        rule: "pipe-download-to-shell",
        risk: Risk::Danger,
        note: "executes a downloaded script without you ever seeing it",
        all_of: &[r"\b(?:curl|wget|fetch)\b[^|]*\|\s*(?:sudo\s+)?(?:ba|z|k|da|fi|c)?sh\b"],
        span_from: 0,
    },
    // ---- privilege ------------------------------------------------------
    // Per the asymmetric-friction rule: bare `sudo` is Caution, because flagging
    // every `sudo apt update` as Danger is exactly the approval fatigue that makes
    // real warnings invisible. `sudo` plus a destructive verb is Danger.
    Pattern {
        rule: "sudo-destructive",
        risk: Risk::Danger,
        note: "runs a destructive command as root",
        all_of: &[r"\bsudo\b", DESTRUCTIVE_VERB],
        span_from: 1,
    },
    Pattern {
        rule: "sudo",
        risk: Risk::Caution,
        note: "runs as root",
        all_of: &[r"\bsudo\b"],
        span_from: 0,
    },
    // ---- resource exhaustion -------------------------------------------
    Pattern {
        rule: "fork-bomb",
        risk: Risk::Danger,
        note: "fork bomb — spawns processes until the machine stops responding",
        all_of: &[r":\s*\(\s*\)\s*\{[^}]*\|[^}]*&[^}]*\}\s*;?\s*:"],
        span_from: 0,
    },
    Pattern {
        rule: "kill-every-process",
        risk: Risk::Danger,
        note: "kills every process you own, ending your session",
        all_of: &[r"\bkill\s+-9\s+-1\b|\bkill\s+-KILL\s+-1\b"],
        span_from: 0,
    },
    // ---- version control ------------------------------------------------
    // `--force(\s|$)` cannot match `--force-with-lease` (the next character is
    // `-`), so the lease form falls through to the Caution rule below instead of
    // firing both and being max()'d up to Danger.
    Pattern {
        rule: "git-force-push",
        risk: Risk::Danger,
        note: "force-push overwrites remote history for everyone",
        all_of: &[
            r"\bgit\b[^|;&]*\bpush\b",
            r"(?:--force(?:\s|$)|\s-f(?:\s|$))",
        ],
        span_from: 1,
    },
    Pattern {
        rule: "git-force-push-with-lease",
        risk: Risk::Caution,
        note: "rewrites remote history, but refuses if someone else pushed first",
        all_of: &[r"\bgit\b[^|;&]*\bpush\b", r"--force-with-lease"],
        span_from: 1,
    },
    Pattern {
        rule: "git-reset-hard",
        risk: Risk::Caution,
        note: "discards uncommitted changes",
        all_of: &[r"\bgit\b[^|;&]*\breset\b", r"--hard"],
        span_from: 1,
    },
    Pattern {
        rule: "git-clean-force",
        risk: Risk::Caution,
        note: "deletes untracked files, which git cannot recover",
        all_of: &[r"\bgit\b[^|;&]*\bclean\b", FORCE_FLAG],
        span_from: 1,
    },
    // ---- data stores ----------------------------------------------------
    Pattern {
        rule: "sql-drop",
        risk: Risk::Danger,
        note: "drops a database, schema or table",
        all_of: &[r"(?i)\bdrop\s+(?:database|schema|table)\b"],
        span_from: 0,
    },
    Pattern {
        rule: "sql-truncate",
        risk: Risk::Danger,
        note: "removes every row in the table",
        all_of: &[r"(?i)\btruncate\s+(?:table\s+)?\w"],
        span_from: 0,
    },
    // Not Danger: `DELETE FROM t WHERE id = 1` is routine, and without lookaround
    // there is no way to tell it from an unbounded delete. Caution is the honest
    // reading.
    Pattern {
        rule: "sql-delete",
        risk: Risk::Caution,
        note: "deletes rows from a database",
        all_of: &[r"(?i)\bdelete\s+from\b"],
        span_from: 0,
    },
    // ---- machine and session state -------------------------------------
    Pattern {
        rule: "power-state-change",
        risk: Risk::Caution,
        note: "shuts down or restarts the machine",
        all_of: &[r"\b(?:shutdown|reboot|halt|poweroff)\b"],
        span_from: 0,
    },
    Pattern {
        rule: "history-clear",
        risk: Risk::Caution,
        note: "erases your shell history",
        all_of: &[r"\bhistory\s+-c\b"],
        span_from: 0,
    },
    Pattern {
        rule: "crontab-remove",
        risk: Risk::Caution,
        note: "deletes all of your scheduled jobs",
        all_of: &[r"\bcrontab\s+-r\b"],
        span_from: 0,
    },
    Pattern {
        rule: "truncate-to-zero",
        risk: Risk::Caution,
        note: "empties the file in place",
        all_of: &[r"\btruncate\b", r"-s\s*0\b"],
        span_from: 1,
    },
];
