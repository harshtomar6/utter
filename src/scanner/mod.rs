//! Local static danger scanner.
//!
//! # This is a UI affordance, NOT a security boundary.
//!
//! It is regex over an unparsed string, and the shell defeats it trivially:
//!
//! ```text
//! X=rm; $X -rf ~                      # the verb lives in a variable
//! RF="-rf"; rm $RF /                  # the flags live in a variable
//! $(echo cm0gLXJmIC8K | base64 -d)    # the whole command is encoded
//! eval "$PAYLOAD"                     # indirection
//! ```
//!
//! Its only job is to make a destructive command *look* destructive before the
//! user presses Enter. It may only ever raise the displayed risk above what the
//! model claimed — never lower it. Do not build any feature that treats a `Safe`
//! verdict here as authoritative, and never gate execution on it.

pub mod patterns;

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::risk::Risk;
use patterns::{Pattern, PATTERNS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub risk: Risk,
    pub note: &'static str,
    /// Byte range in the scanned command, for highlighting the offending
    /// fragment in the danger warning.
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    /// The highest risk any rule assigned. `Safe` when nothing matched — that is
    /// the scanner's *opinion*, combined via `max` with the model's own claim.
    pub risk: Risk,
    /// Highest risk first.
    pub findings: Vec<Finding>,
}

impl ScanResult {
    pub fn worst(&self) -> Option<&Finding> {
        self.findings.first()
    }
}

/// Compiled once per process. A bad regex in the table is a programming error, so
/// it is caught by the `every_pattern_compiles` test rather than handled here —
/// but an unparseable expression is skipped rather than panicking a user's shell.
static COMPILED: LazyLock<Vec<(&'static Pattern, Vec<Regex>)>> = LazyLock::new(|| {
    PATTERNS
        .iter()
        .filter_map(|pattern| {
            let regexes: Option<Vec<Regex>> =
                pattern.all_of.iter().map(|r| Regex::new(r).ok()).collect();
            regexes.map(|r| (pattern, r))
        })
        .collect()
});

pub fn scan(command: &str) -> ScanResult {
    let mut findings = Vec::new();

    for (pattern, regexes) in COMPILED.iter() {
        let mut spans = Vec::with_capacity(regexes.len());
        for re in regexes {
            match re.find(command) {
                Some(m) => spans.push(m.range()),
                // Every regex in `all_of` must match for the rule to fire.
                None => break,
            }
        }
        if spans.len() != regexes.len() {
            continue;
        }
        let span = spans
            .get(pattern.span_from)
            .cloned()
            .unwrap_or_else(|| spans[0].clone());
        findings.push(Finding {
            rule: pattern.rule,
            risk: pattern.risk,
            note: pattern.note,
            span,
        });
    }

    findings.sort_by(|a, b| b.risk.cmp(&a.risk).then_with(|| a.rule.cmp(b.rule)));
    let risk = findings.first().map(|f| f.risk).unwrap_or(Risk::Safe);
    ScanResult { risk, findings }
}

/// Displayed risk is always the more severe of the two opinions. The scanner can
/// escalate a model that understated the danger; it can never talk one down.
pub fn effective_risk(model: Risk, scanner: Risk) -> Risk {
    model.max(scanner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn risk_of(cmd: &str) -> Risk {
        scan(cmd).risk
    }

    fn fires(cmd: &str, rule: &str) -> bool {
        scan(cmd).findings.iter().any(|f| f.rule == rule)
    }

    // ---- table integrity ------------------------------------------------

    #[test]
    fn every_pattern_compiles() {
        for p in PATTERNS {
            for expr in p.all_of {
                assert!(
                    Regex::new(expr).is_ok(),
                    "rule {} has an invalid regex: {expr}",
                    p.rule
                );
            }
        }
        assert_eq!(
            COMPILED.len(),
            PATTERNS.len(),
            "a pattern was silently dropped at compile time"
        );
    }

    #[test]
    fn every_span_from_is_in_range() {
        for p in PATTERNS {
            assert!(
                p.span_from < p.all_of.len(),
                "rule {} has span_from {} but only {} regexes",
                p.rule,
                p.span_from,
                p.all_of.len()
            );
        }
    }

    #[test]
    fn every_rule_name_is_unique() {
        let mut names: Vec<_> = PATTERNS.iter().map(|p| p.rule).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate rule name in the table");
    }

    #[test]
    fn every_rule_has_a_human_note() {
        for p in PATTERNS {
            assert!(!p.note.is_empty(), "rule {} has no note", p.rule);
            assert_ne!(p.risk, Risk::Safe, "rule {} would never escalate", p.rule);
        }
    }

    // ---- the patterns the spec requires ---------------------------------

    #[test]
    fn rm_rf_root_is_danger() {
        assert_eq!(risk_of("rm -rf /"), Risk::Danger);
        assert_eq!(risk_of("rm -fr /"), Risk::Danger);
        assert_eq!(risk_of("rm -rf /*"), Risk::Danger);
        assert_eq!(risk_of("rm --recursive --force /"), Risk::Danger);
        assert!(fires("rm -rf /", "rm-recursive-critical-target"));
    }

    #[test]
    fn rm_rf_home_is_danger() {
        assert_eq!(risk_of("rm -rf ~"), Risk::Danger);
        assert_eq!(risk_of("rm -rf $HOME"), Risk::Danger);
        assert_eq!(risk_of("rm -rf ${HOME}"), Risk::Danger);
        assert_eq!(risk_of("rm -rf ~/*"), Risk::Danger);
    }

    #[test]
    fn rm_rf_system_path_is_danger() {
        assert_eq!(risk_of("rm -rf /usr/local/lib"), Risk::Danger);
        assert_eq!(risk_of("rm -rf /etc"), Risk::Danger);
        assert_eq!(risk_of("sudo rm -rf /System/Library"), Risk::Danger);
    }

    #[test]
    fn rm_rf_of_a_named_subdirectory_is_only_caution() {
        // The whole point of the asymmetric-friction rule: this is routine.
        assert_eq!(risk_of("rm -rf ./target"), Risk::Caution);
        assert_eq!(risk_of("rm -rf node_modules"), Risk::Caution);
        assert_eq!(risk_of("rm -rf ~/Downloads/tmp"), Risk::Caution);
    }

    #[test]
    fn dd_patterns() {
        assert_eq!(risk_of("dd if=disk.img of=/dev/disk2 bs=1m"), Risk::Danger);
        assert_eq!(risk_of("dd if=/dev/zero of=/dev/sda"), Risk::Danger);
        assert_eq!(risk_of("dd if=a of=b.img"), Risk::Caution);
    }

    #[test]
    fn mkfs_is_danger() {
        assert_eq!(risk_of("mkfs.ext4 /dev/sdb1"), Risk::Danger);
        assert_eq!(risk_of("mkfs -t ext4 /dev/sdb1"), Risk::Danger);
    }

    #[test]
    fn fork_bomb_is_danger() {
        assert_eq!(risk_of(":(){ :|:& };:"), Risk::Danger);
        assert_eq!(risk_of(":() { :|: & }; :"), Risk::Danger);
        assert!(fires(":(){ :|:& };:", "fork-bomb"));
    }

    #[test]
    fn pipe_download_to_shell_is_danger() {
        assert_eq!(risk_of("curl -fsSL https://x.sh | sh"), Risk::Danger);
        assert_eq!(risk_of("curl https://x.sh | bash"), Risk::Danger);
        assert_eq!(risk_of("wget -qO- https://x.sh | sh"), Risk::Danger);
        assert_eq!(risk_of("curl https://x.sh | sudo bash"), Risk::Danger);
        assert_eq!(risk_of("curl https://x.sh | zsh"), Risk::Danger);
    }

    #[test]
    fn downloading_without_piping_to_a_shell_is_not_flagged() {
        assert_eq!(risk_of("curl -fsSL https://x.sh -o install.sh"), Risk::Safe);
        assert_eq!(
            risk_of("curl -s https://api.example.com | jq ."),
            Risk::Safe
        );
    }

    #[test]
    fn chmod_recursive_patterns() {
        assert_eq!(risk_of("chmod -R 755 /usr/local"), Risk::Danger);
        assert_eq!(risk_of("chmod -R 777 /"), Risk::Danger);
        assert_eq!(risk_of("chmod -R 755 ./build"), Risk::Caution);
        assert_eq!(risk_of("chmod 777 ./f"), Risk::Caution);
    }

    #[test]
    fn writing_over_a_block_device_is_danger() {
        assert_eq!(risk_of("echo x > /dev/sda"), Risk::Danger);
        assert_eq!(risk_of("cat img > /dev/disk2"), Risk::Danger);
    }

    #[test]
    fn dev_null_and_friends_are_not_block_devices() {
        assert_eq!(risk_of("make 2> /dev/null"), Risk::Safe);
        assert_eq!(risk_of("head -c 16 /dev/urandom | xxd"), Risk::Safe);
    }

    #[test]
    fn redirect_over_a_system_path_is_danger() {
        assert_eq!(risk_of("echo x > /etc/hosts"), Risk::Danger);
        assert_eq!(risk_of("cat a >> /usr/share/f"), Risk::Danger);
    }

    #[test]
    fn git_force_push_is_danger_but_with_lease_is_caution() {
        assert_eq!(risk_of("git push --force origin main"), Risk::Danger);
        assert_eq!(risk_of("git push -f"), Risk::Danger);
        assert_eq!(
            risk_of("git push --force-with-lease origin main"),
            Risk::Caution
        );
        assert!(!fires("git push --force-with-lease", "git-force-push"));
    }

    #[test]
    fn ordinary_git_push_is_safe() {
        assert_eq!(risk_of("git push origin main"), Risk::Safe);
    }

    #[test]
    fn sudo_alone_is_caution_not_danger() {
        // Approval fatigue is the real failure mode of this category. Flagging
        // every routine sudo as Danger would make the loud warning meaningless.
        assert_eq!(risk_of("sudo apt update"), Risk::Caution);
        assert_eq!(risk_of("sudo systemctl restart nginx"), Risk::Caution);
        assert!(fires("sudo apt update", "sudo"));
    }

    #[test]
    fn sudo_plus_a_destructive_verb_is_danger() {
        assert_eq!(risk_of("sudo rm ./file"), Risk::Danger);
        assert_eq!(risk_of("sudo dd if=a of=b"), Risk::Danger);
        assert_eq!(risk_of("sudo chown -R me /var"), Risk::Danger);
        assert!(fires("sudo rm ./file", "sudo-destructive"));
    }

    // ---- other rules ----------------------------------------------------

    #[test]
    fn sql_drop_and_truncate_are_danger_but_bounded_delete_is_caution() {
        assert_eq!(risk_of("psql -c 'DROP TABLE users'"), Risk::Danger);
        assert_eq!(risk_of("mysql -e 'truncate table logs'"), Risk::Danger);
        assert_eq!(
            risk_of("psql -c 'delete from users where id = 1'"),
            Risk::Caution
        );
    }

    #[test]
    fn destructive_git_and_find_operations_are_caution() {
        assert_eq!(risk_of("git reset --hard HEAD~1"), Risk::Caution);
        assert_eq!(risk_of("git clean -fdx"), Risk::Caution);
        assert_eq!(risk_of("find . -name '*.tmp' -delete"), Risk::Caution);
        assert_eq!(risk_of("find . -name '*.log' -exec rm {} +"), Risk::Caution);
    }

    #[test]
    fn session_and_machine_state_rules() {
        assert_eq!(risk_of("history -c"), Risk::Caution);
        assert_eq!(risk_of("crontab -r"), Risk::Caution);
        assert_eq!(risk_of("shutdown -h now"), Risk::Caution);
        assert_eq!(risk_of("kill -9 -1"), Risk::Danger);
        assert_eq!(risk_of("truncate -s 0 app.log"), Risk::Caution);
    }

    // ---- the read-only baseline ----------------------------------------

    #[test]
    fn ordinary_read_only_commands_stay_silent() {
        // If these ever start firing, warnings stop meaning anything.
        for cmd in [
            "ls -la",
            "ps axo pid,rss,comm | sort -nrk2",
            "rg TODO --stats",
            "git status",
            "git log --oneline -20",
            "du -sh * | sort -h",
            "jq '.items[] | .name' data.json",
            "docker ps -a",
            "kubectl get pods -A",
            "cat README.md",
            "find . -name '*.rs' -type f",
            "grep -rn panic src/",
            "tar -xzf archive.tar.gz",
            "lsof -i :3000",
            "docker run --rm -it ubuntu bash",
            "docker build --force-rm -t app .",
            "yarn install --force",
            "npm rm left-pad",
            "git stash drop",
            "git branch -D old",
            "rsync -av --delete src/ dst/",
        ] {
            assert_eq!(risk_of(cmd), Risk::Safe, "false positive on: {cmd}");
        }
    }

    // ---- combination rule ----------------------------------------------

    #[test]
    fn effective_risk_takes_the_worse_of_the_two_opinions() {
        assert_eq!(effective_risk(Risk::Safe, Risk::Danger), Risk::Danger);
        assert_eq!(effective_risk(Risk::Danger, Risk::Safe), Risk::Danger);
        assert_eq!(effective_risk(Risk::Safe, Risk::Safe), Risk::Safe);
        assert_eq!(effective_risk(Risk::Caution, Risk::Safe), Risk::Caution);
    }

    #[test]
    fn scanner_can_escalate_a_model_that_understated_the_risk() {
        let claimed = Risk::Safe;
        let scanned = scan("rm -rf /").risk;
        assert_eq!(effective_risk(claimed, scanned), Risk::Danger);
    }

    // ---- spans ----------------------------------------------------------

    #[test]
    fn span_points_at_the_destructive_fragment() {
        let cmd = "rm -rf /";
        let result = scan(cmd);
        let worst = result.worst().unwrap();
        assert_eq!(worst.rule, "rm-recursive-critical-target");
        assert_eq!(&cmd[worst.span.clone()], " /");
    }

    #[test]
    fn spans_are_valid_byte_ranges_into_the_command() {
        for cmd in [
            "rm -rf /",
            "sudo chmod -R 777 /etc",
            "curl x | sh",
            "git push -f",
            ":(){ :|:& };:",
        ] {
            for f in scan(cmd).findings {
                assert!(cmd.get(f.span.clone()).is_some(), "bad span in {}", f.rule);
            }
        }
    }

    #[test]
    fn findings_are_ordered_worst_first() {
        let result = scan("sudo rm -rf /");
        assert_eq!(result.findings[0].risk, Risk::Danger);
        let mut previous = Risk::Danger;
        for f in &result.findings {
            assert!(f.risk <= previous);
            previous = f.risk;
        }
    }

    #[test]
    fn empty_and_whitespace_commands_are_safe_not_a_panic() {
        assert_eq!(risk_of(""), Risk::Safe);
        assert_eq!(risk_of("   "), Risk::Safe);
    }

    #[test]
    fn multibyte_input_does_not_panic_and_spans_stay_valid() {
        let cmd = "echo 'héllo wörld' && rm -rf /";
        let result = scan(cmd);
        assert_eq!(result.risk, Risk::Danger);
        for f in result.findings {
            assert!(cmd.get(f.span.clone()).is_some());
        }
    }

    // ---- documented limits ---------------------------------------------

    #[test]
    fn known_bypasses_are_not_detected_and_that_is_expected() {
        // Documenting the boundary in an executable form. If any of these ever
        // starts matching, the scanner has grown a parser and this test should be
        // revisited deliberately — not silently deleted.
        assert_eq!(risk_of("X=rm; $X -rf ~"), Risk::Safe);
        assert_eq!(risk_of("RF=\"-rf\"; rm $RF /"), Risk::Safe);
        assert_eq!(risk_of("$(echo cm0gLXJmIC8K | base64 -d)"), Risk::Safe);
        assert_eq!(risk_of("eval \"$PAYLOAD\""), Risk::Safe);
    }

    #[test]
    fn rm_as_a_flag_is_not_rm_as_a_command() {
        // `docker run --rm` and `docker build --force-rm` are among the most
        // common commands a developer types. A bare `\brm\b` matched both, and
        // `sudo docker run --rm` escalated all the way to Danger. Training the
        // user to dismiss the label is the failure mode that matters here.
        assert_eq!(risk_of("docker run --rm -it ubuntu bash"), Risk::Safe);
        assert_eq!(risk_of("docker build --force-rm -t app ."), Risk::Safe);
        assert_eq!(risk_of("sudo docker run --rm alpine"), Risk::Caution);
        // The real thing still fires.
        assert_eq!(risk_of("docker run alpine; rm -rf /"), Risk::Danger);
    }
}
