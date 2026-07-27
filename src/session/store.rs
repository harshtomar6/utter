//! Disk IO for session files and shell-state sidecars.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

/// `None` when the file does not exist — the ordinary first-run case, not an error.
pub fn read_to_string(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    }
}

/// Write to a temporary sibling then rename.
///
/// Two shells sharing a session file is unlikely but possible, and a half-written
/// JSON file would break every subsequent invocation in that terminal. `rename` on
/// the same filesystem is atomic, so a reader sees either the old file or the new
/// one.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

    // The pid keeps concurrent writers from clobbering each other's temp file.
    let temp: PathBuf = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temp, contents).with_context(|| format!("writing {}", temp.display()))?;

    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Do not leave the temp file behind on failure.
            let _ = std::fs::remove_file(&temp);
            Err(anyhow::Error::new(e).context(format!("replacing {}", path.display())))
        }
    }
}

/// `true` when something was removed.
pub fn remove_if_exists(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(anyhow::Error::new(e).context(format!("removing {}", path.display()))),
    }
}

/// How long ago the file was last written.
///
/// The shell hooks carry no timestamp of their own — mtime is the timestamp, which
/// is why the hooks can stay pure builtins with no `date` spawn.
pub fn age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok()
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("utter-store-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reading_a_missing_file_is_none_not_an_error() {
        let dir = temp_dir("missing");
        assert!(read_to_string(&dir.join("nope.json")).unwrap().is_none());
    }

    #[test]
    fn write_atomic_creates_parent_directories() {
        let dir = temp_dir("mkdir");
        let path = dir.join("nested/deeper/session.json");
        write_atomic(&path, "{}").unwrap();
        assert_eq!(read_to_string(&path).unwrap().as_deref(), Some("{}"));
    }

    #[test]
    fn write_atomic_overwrites_and_leaves_no_temp_files() {
        let dir = temp_dir("overwrite");
        let path = dir.join("s.json");
        write_atomic(&path, "first").unwrap();
        write_atomic(&path, "second").unwrap();
        assert_eq!(read_to_string(&path).unwrap().as_deref(), Some("second"));

        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "left temp files behind: {strays:?}");
    }

    #[test]
    fn remove_reports_whether_anything_was_there() {
        let dir = temp_dir("remove");
        let path = dir.join("s.json");
        assert!(!remove_if_exists(&path).unwrap());
        write_atomic(&path, "x").unwrap();
        assert!(remove_if_exists(&path).unwrap());
        assert!(!remove_if_exists(&path).unwrap());
    }

    #[test]
    fn age_is_small_for_a_file_just_written() {
        let dir = temp_dir("age");
        let path = dir.join("s.state");
        write_atomic(&path, "x").unwrap();
        assert!(age(&path).unwrap() < Duration::from_secs(60));
        assert!(age(&dir.join("absent")).is_none());
    }
}
