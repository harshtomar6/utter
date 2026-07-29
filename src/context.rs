use std::fmt::Write as _;

/// Which coreutils dialect the machine speaks.
///
/// This is the single highest-value fact in the whole system prompt. `ps`, `sed`,
/// `find`, `stat` and `date` take incompatible flags between the two, and a
/// GNU-flavoured command run on macOS is the most common way this class of tool
/// produces something broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Bsd,
    Gnu,
    Unknown,
}

impl Flavor {
    /// `msys` is the MSYSTEM environment variable, set by Git Bash and MSYS2.
    ///
    /// It has to be consulted because `std::env::consts::OS` only says
    /// "windows", while the shell we are actually serving ships GNU coreutils —
    /// so `sed -i` takes no suffix, `stat -c` works, and `date -d` parses. Going
    /// by the OS alone would hand a Git Bash user portable-POSIX advice and lose
    /// every GNU-specific flag that does in fact work there.
    fn detect(os: &str, msys: Option<&str>) -> Self {
        if os == "windows" {
            return match msys {
                Some(m) if !m.is_empty() => Flavor::Gnu,
                _ => Flavor::Unknown,
            };
        }
        match os {
            "macos" | "freebsd" | "openbsd" | "netbsd" | "dragonfly" | "ios" => Flavor::Bsd,
            "linux" | "android" => Flavor::Gnu,
            _ => Flavor::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Flavor::Bsd => "BSD",
            Flavor::Gnu => "GNU",
            Flavor::Unknown => "unknown",
        }
    }

    /// Concrete flag differences, not vague advice. Vague advice does not stop a
    /// model from writing `sed -i` on macOS.
    pub fn guidance(self) -> &'static str {
        match self {
            Flavor::Bsd => concat!(
                "- `sed -i` REQUIRES a backup-suffix argument: `sed -i '' 's/a/b/' f`\n",
                "- `ps` uses BSD syntax: `ps axo pid,rss,comm` (NOT `ps -eo`)\n",
                "- `stat -f '%z'` (NOT `stat -c '%s'`)\n",
                "- `date -v-1d` / `date -r <epoch>` (NOT `date -d`)\n",
                "- `find` has no `-printf`; use `-exec` or pipe to `xargs`\n",
                "- `grep` has no `-P`; use `-E` or `rg` if available\n",
                "- `readlink -f` is unavailable on older macOS; prefer `cd ... && pwd -P`\n",
                "- most tools accept only short flags, not GNU `--long-form`",
            ),
            Flavor::Gnu => concat!(
                "- `sed -i 's/a/b/' f` takes no suffix argument\n",
                "- `ps -eo pid,rss,comm --sort=-rss`\n",
                "- `stat -c '%s'`\n",
                "- `date -d '1 day ago'`\n",
                "- `find -printf` and `grep -P` are available",
            ),
            Flavor::Unknown => "- coreutils dialect unknown; prefer POSIX-portable flags only",
        }
    }

    /// Which human-readable-output tools actually exist on this dialect.
    ///
    /// Kept separate from `guidance` because the failure mode is different: that
    /// one stops the command from running at all, this one stops it from
    /// answering the question. `numfmt` is GNU-only and its absence on macOS is
    /// the trap worth naming — it is the obvious reach for formatting bytes.
    pub fn output_guidance(self) -> &'static str {
        match self {
            Flavor::Bsd => concat!(
                "Human-readable output on this machine:\n",
                "- `du -h`, `ls -lh`, `df -h` are available; `sort -h` sorts those suffixes\n",
                "- for largest-files, `find ... -exec du -h {} + | sort -rh | head` beats \
                 `stat -f %z` piped to `sort -rn`, which prints undecorated bytes\n",
                "- there is NO `numfmt` here (GNU only); use a `-h` flag or `awk` arithmetic\n",
                "- `ps` reports RSS in kilobytes: divide in `awk` and label the unit\n",
            ),
            Flavor::Gnu => concat!(
                "Human-readable output on this machine:\n",
                "- `du -h`, `ls -lh`, `df -h`, `sort -h` and `numfmt --to=iec` are all available\n",
                "- `ps` reports RSS in kilobytes: divide in `awk` or pipe through `numfmt`\n",
            ),
            Flavor::Unknown => "Prefer `-h` style flags for sizes where the tool offers them.\n",
        }
    }
}

/// Tools worth telling the model about. Looked up via PATH only — `which` does no
/// process spawning, so probing this list is effectively free and keeps startup
/// imperceptible.
const PROBED: &[&str] = &[
    "rg",
    "fd",
    "jq",
    "yq",
    "fzf",
    "git",
    "curl",
    "wget",
    "tar",
    "zip",
    "unzip",
    "python3",
    "node",
    "docker",
    "kubectl",
    "brew",
    "awk",
    "gawk",
    "sed",
    "gsed",
    "find",
    "gfind",
    "gstat",
    "gdate",
    "htop",
    "lsof",
    "dig",
    "ss",
    "netstat",
    "systemctl",
    "launchctl",
    "ffmpeg",
    "rsync",
];

/// GNU builds installed alongside BSD ones under a `g` prefix (Homebrew
/// coreutils). Worth surfacing separately: if `gsed` exists the model can reach
/// for GNU semantics deliberately instead of guessing.
const G_PREFIXED: &[&str] = &["gsed", "gfind", "gstat", "gdate", "gawk"];

#[derive(Debug, Clone)]
pub struct ShellContext {
    pub os: String,
    pub arch: String,
    pub flavor: Flavor,
    pub shell: String,
    pub cwd: String,
    pub tools: Vec<&'static str>,
    pub gnu_prefixed: Vec<&'static str>,
}

impl ShellContext {
    /// Rebuilt on every invocation — cwd, `$SHELL` and PATH all change between
    /// runs, which is exactly why the system prompt is never persisted.
    pub fn probe() -> Self {
        let os_key = std::env::consts::OS;
        let msystem = std::env::var("MSYSTEM").ok();
        let tools: Vec<&'static str> = PROBED
            .iter()
            .copied()
            .filter(|t| which::which(t).is_ok())
            .collect();

        Self {
            os: pretty_os(os_key, msystem.as_deref()),
            arch: std::env::consts::ARCH.to_string(),
            flavor: Flavor::detect(os_key, msystem.as_deref()),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "unknown".into()),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "unknown".into()),
            gnu_prefixed: G_PREFIXED
                .iter()
                .copied()
                .filter(|t| tools.contains(t))
                .collect(),
            tools,
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "OS: {} ({})", self.os, self.arch);
        let _ = writeln!(out, "Coreutils dialect: {}", self.flavor.label());
        let _ = writeln!(out, "Shell: {}", self.shell);
        let _ = writeln!(out, "Working directory: {}", self.cwd);
        let _ = writeln!(out, "Tools on PATH: {}", self.tools.join(", "));
        if !self.gnu_prefixed.is_empty() {
            let _ = writeln!(
                out,
                "GNU builds also available under a g- prefix: {}",
                self.gnu_prefixed.join(", ")
            );
        }
        out
    }
}

/// No exact OS version. Reading it costs a `sw_vers` spawn on macOS for a fact
/// that almost never changes command syntax — the BSD/GNU split already carries
/// that signal, and startup latency is a product requirement.
fn pretty_os(os_key: &str, msys: Option<&str>) -> String {
    match os_key {
        "macos" => "macOS".to_string(),
        "linux" => linux_pretty_name().unwrap_or_else(|| "Linux".to_string()),
        // Name the environment, not just the OS: a command that works in Git Bash
        // may be meaningless in PowerShell, and the model should know which it is
        // writing for.
        "windows" => match msys {
            Some(m) if !m.is_empty() => format!("Windows ({m}, MSYS2/Git Bash environment)"),
            _ => "Windows".to_string(),
        },
        other => other.to_string(),
    }
}

/// `/etc/os-release` is a plain file read, so this one is free.
fn linux_pretty_name() -> Option<String> {
    let raw = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_is_bsd_and_linux_is_gnu() {
        assert_eq!(Flavor::detect("macos", None), Flavor::Bsd);
        assert_eq!(Flavor::detect("linux", None), Flavor::Gnu);
        assert_eq!(Flavor::detect("plan9", None), Flavor::Unknown);
    }

    #[test]
    fn git_bash_is_gnu_despite_running_on_windows() {
        // MSYS2 ships GNU coreutils, so GNU flags are the correct advice there.
        assert_eq!(Flavor::detect("windows", Some("MINGW64")), Flavor::Gnu);
        assert_eq!(Flavor::detect("windows", Some("MSYS")), Flavor::Gnu);
    }

    #[test]
    fn windows_without_msys_stays_unknown() {
        // PowerShell and cmd are not served by this tool; claiming a dialect
        // would produce confidently wrong flags.
        assert_eq!(Flavor::detect("windows", None), Flavor::Unknown);
        assert_eq!(Flavor::detect("windows", Some("")), Flavor::Unknown);
    }

    #[test]
    fn the_os_label_names_the_msys_environment() {
        let label = pretty_os("windows", Some("MINGW64"));
        assert!(label.contains("MINGW64"));
        assert!(label.contains("Git Bash"));
        assert_eq!(pretty_os("windows", None), "Windows");
    }

    #[test]
    fn bsd_guidance_names_the_empty_sed_suffix() {
        assert!(Flavor::Bsd.guidance().contains("sed -i ''"));
        assert!(!Flavor::Gnu.guidance().contains("sed -i ''"));
    }

    #[test]
    fn probe_reports_the_current_machine() {
        let ctx = ShellContext::probe();
        assert!(!ctx.os.is_empty());
        assert_ne!(ctx.flavor, Flavor::Unknown, "test host should be mac/linux");
        let rendered = ctx.render();
        assert!(rendered.contains("Coreutils dialect:"));
        assert!(rendered.contains("Working directory:"));
    }

    #[test]
    fn g_prefixed_is_a_subset_of_detected_tools() {
        let ctx = ShellContext::probe();
        for g in &ctx.gnu_prefixed {
            assert!(ctx.tools.contains(g));
        }
    }
}
