//! Types shared by the readers, the prices, and the report.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// Where a session was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// Droid CLI sessions under `~/.factory/sessions`.
    Droid,
    /// `OpenCode` sessions in its `SQLite` database.
    OpenCode,
    /// agy (Antigravity CLI) conversations.
    Agy,
    /// Codex CLI rollout logs under `~/.codex`.
    Codex,
    /// Pi sessions under `~/.pi/agent/sessions`.
    Pi,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Droid => "droid",
            Self::OpenCode => "opencode",
            Self::Agy => "agy",
            Self::Codex => "codex",
            Self::Pi => "pi",
        })
    }
}

/// Token counts for one session. Reasoning tokens are not tracked; each source reports them differently.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    /// Prompt tokens that were not served from cache.
    pub input: u64,
    /// Completion tokens.
    pub output: u64,
    /// Prompt tokens served from cache.
    pub cache_read: u64,
    /// Prompt tokens written to cache.
    pub cache_write: u64,
}

impl Tokens {
    /// All four counts added together, stopping at `u64::MAX`.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Adds `other` to these counts, stopping each count at `u64::MAX`.
    pub const fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
    }
}

/// A count as `f64`, rounded the way `as` rounds. Each 32-bit half converts exactly with `f64::from`, so this needs no `as` cast.
pub(crate) fn count_f64(count: u64) -> f64 {
    // Both halves fit in 32 bits, so the fallbacks never run.
    let high = u32::try_from(count >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(count & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

/// One coding session, as read from its source.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Where the session was recorded.
    pub source: Source,
    /// When the session started (UTC).
    pub started: DateTime<Utc>,
    /// Project directory, when the source records one.
    pub project: Option<String>,
    /// Model name, when the source records one.
    pub model: Option<String>,
    /// Token counts.
    pub tokens: Tokens,
    /// Cost in USD: the source's recorded cost if it has one, else an estimate from the price table. `None` when neither exists.
    pub cost_usd: Option<f64>,
}

/// Sessions found by one reader, plus what could not be read.
#[derive(Debug, Default)]
pub struct Read {
    /// Sessions that were read.
    pub sessions: Vec<Session>,
    /// Entries that looked like sessions but could not be read.
    pub skipped: usize,
    /// Sessions read without a model name because their model lookup failed. They still count, with no model.
    pub models_unreadable: usize,
}

/// Files found under a folder, and how many folders or entries under it could not be read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Matching files, sorted.
    pub files: Vec<PathBuf>,
    /// Folders and entries skipped because they could not be read.
    pub unreadable: usize,
}

/// Every file under `root` whose name ends with `suffix`, searched recursively. A folder or entry under `root` that cannot be read is skipped and counted.
///
/// # Errors
///
/// Returns an error, naming `root`, when `root` itself cannot be read.
pub fn files_with_suffix(root: &Path, suffix: &str) -> io::Result<Found> {
    // Opening the root first makes a missing or unreadable root an error. Folders below it are skipped.
    drop(
        std::fs::read_dir(root)
            .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", root.display())))?,
    );
    let mut found = Found::default();
    let mut seen = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        // A folder reached through a symlink, or a link back up the tree, is read once.
        // If the folder cannot be canonicalized, it is still checked as given, and the read below counts it.
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !seen.insert(canonical) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            found.unreadable += 1;
            continue;
        };
        sort_entries(
            entries.map(|entry| entry.map(|entry| entry.path())),
            suffix,
            &mut found,
            &mut pending,
        );
    }
    found.files.sort();
    Ok(found)
}

/// Sorts one folder's entries: subfolders are queued in `pending` and matching files go to `found`. An entry that cannot be read is counted as unreadable.
fn sort_entries(
    entries: impl Iterator<Item = io::Result<PathBuf>>,
    suffix: &str,
    found: &mut Found,
    pending: &mut Vec<PathBuf>,
) {
    for entry in entries {
        let Ok(path) = entry else {
            found.unreadable += 1;
            continue;
        };
        if path.is_dir() {
            pending.push(path);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix))
        {
            found.files.push(path);
        }
    }
}

/// Adds one source's sessions to `sessions`. Returns a warning when the source could not be read or skipped entries.
pub fn add_source<E: fmt::Display>(
    name: &str,
    result: Result<Read, E>,
    sessions: &mut Vec<Session>,
) -> Option<String> {
    match result {
        Ok(read) => {
            let mut notes = Vec::new();
            if read.skipped > 0 {
                notes.push(format!("skipped {} unreadable entries", read.skipped));
            }
            if read.models_unreadable > 0 {
                notes.push(format!(
                    "could not read the model of {} sessions",
                    read.models_unreadable
                ));
            }
            sessions.extend(read.sessions);
            (!notes.is_empty()).then(|| format!("{name}: {}", notes.join("; ")))
        }
        Err(err) => Some(format!("{name}: {err}")),
    }
}

/// Parses an RFC 3339 timestamp into UTC. Returns `None` for any other text.
#[must_use]
pub fn parse_timestamp(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::testing::ScratchDir;

    #[test]
    fn counts_convert_to_f64_with_correct_rounding() {
        // Exact values, and the rounding `as` would give: 2^53 + 1 is a tie that rounds to even, and u64::MAX rounds up to 2^64.
        assert_eq!(count_f64(0).to_bits(), 0.0_f64.to_bits());
        assert_eq!(
            count_f64(12_345_678_901).to_bits(),
            12_345_678_901.0_f64.to_bits()
        );
        assert_eq!(count_f64(1 << 32).to_bits(), 4_294_967_296.0_f64.to_bits());
        assert_eq!(
            count_f64(1 << 53).to_bits(),
            9_007_199_254_740_992.0_f64.to_bits()
        );
        assert_eq!(
            count_f64((1 << 53) + 1).to_bits(),
            9_007_199_254_740_992.0_f64.to_bits()
        );
        assert_eq!(
            count_f64(u64::MAX).to_bits(),
            18_446_744_073_709_551_616.0_f64.to_bits()
        );
    }

    #[test]
    fn tokens_total_and_add() {
        let mut tokens = Tokens {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
        };
        assert_eq!(
            tokens.total(),
            10,
            "the token total sums all four token kinds"
        );
        tokens.add(&Tokens {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
        });
        assert_eq!(
            tokens,
            Tokens {
                input: 11,
                output: 22,
                cache_read: 33,
                cache_write: 44
            },
            "adding token counts sums each kind separately"
        );
    }

    #[test]
    fn token_sums_stop_at_the_maximum_instead_of_overflowing() {
        let full = Tokens {
            input: u64::MAX,
            output: u64::MAX,
            cache_read: u64::MAX,
            cache_write: u64::MAX,
        };
        assert_eq!(
            full.total(),
            u64::MAX,
            "a total larger than u64::MAX stops at u64::MAX"
        );
        let mut tokens = Tokens {
            input: u64::MAX,
            ..Tokens::default()
        };
        tokens.add(&Tokens {
            input: 1,
            ..Tokens::default()
        });
        assert_eq!(
            tokens.input,
            u64::MAX,
            "adding past u64::MAX stops at u64::MAX"
        );
    }

    fn one_session() -> Session {
        Session {
            source: Source::Agy,
            started: parse_timestamp("2026-10-25T00:00:00Z").unwrap(),
            project: None,
            model: None,
            tokens: Tokens::default(),
            cost_usd: None,
        }
    }

    #[test]
    fn a_readable_source_adds_its_sessions_and_warns_only_about_skips() {
        let mut sessions = Vec::new();
        let clean = Read {
            sessions: vec![one_session()],
            ..Read::default()
        };
        assert_eq!(
            add_source::<io::Error>("agy", Ok(clean), &mut sessions),
            None,
            "a readable source with no skips adds no warning"
        );
        assert_eq!(
            sessions.len(),
            1,
            "the readable source session is added to the list"
        );

        let skipped = Read {
            sessions: vec![one_session()],
            skipped: 2,
            ..Read::default()
        };
        assert_eq!(
            add_source::<io::Error>("agy", Ok(skipped), &mut sessions),
            Some("agy: skipped 2 unreadable entries".to_owned()),
            "skipped entries produce a warning naming the source and count"
        );
        assert_eq!(
            sessions.len(),
            2,
            "sessions from both reads accumulate in the list"
        );
    }

    #[test]
    fn unreadable_models_are_warned_about_alongside_skips() {
        let mut sessions = Vec::new();
        let read = Read {
            sessions: vec![one_session()],
            skipped: 1,
            models_unreadable: 3,
        };
        assert_eq!(
            add_source::<io::Error>("agy", Ok(read), &mut sessions),
            Some(
                "agy: skipped 1 unreadable entries; could not read the model of 3 sessions"
                    .to_owned()
            ),
            "both kinds of trouble appear in one warning naming the source"
        );
        assert_eq!(
            sessions.len(),
            1,
            "sessions with an unreadable model are still added"
        );
    }

    #[test]
    fn an_unreadable_source_adds_nothing_and_names_itself() {
        let mut sessions = Vec::new();
        let err = io::Error::other("no such folder");
        assert_eq!(
            add_source("droid", Err(err), &mut sessions),
            Some("droid: no such folder".to_owned()),
            "an unreadable source reports its error under its own name"
        );
        assert_eq!(
            sessions,
            Vec::<Session>::new(),
            "an unreadable source adds no sessions"
        );
    }

    #[test]
    fn timestamps_accept_offsets_and_reject_text() {
        assert_eq!(
            parse_timestamp("2026-10-05T17:04:35+02:00"),
            parse_timestamp("2026-10-05T15:04:35Z"),
            "the same instant with different offsets parses to equal values"
        );
        assert!(
            parse_timestamp("yesterday").is_none(),
            "text that is not a timestamp does not parse"
        );
    }

    #[test]
    fn files_are_found_recursively_by_suffix() {
        let root = ScratchDir::new("files-by-suffix");
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("one.settings.json"), "{}").unwrap();
        fs::write(nested.join("two.settings.json"), "{}").unwrap();
        fs::write(nested.join("three.jsonl"), "").unwrap();

        let found = files_with_suffix(&root, ".settings.json");

        let mut names: Vec<String> = found
            .unwrap()
            .files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["one.settings.json", "two.settings.json"],
            "files are found in nested folders by suffix only"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_reached_through_a_symlink_is_read_once() {
        let root = ScratchDir::new("symlinked-folder");
        let real = root.join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("a.jsonl"), "").unwrap();
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();

        let found = files_with_suffix(&root, ".jsonl").unwrap();

        assert_eq!(found.files.len(), 1, "found {found:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_ends_and_still_finds_the_files() {
        let root = ScratchDir::new("symlink-loop");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("a.jsonl"), "").unwrap();
        std::os::unix::fs::symlink(&root, nested.join("back")).unwrap();
        std::os::unix::fs::symlink(&nested, root.join("again")).unwrap();

        let found = files_with_suffix(&root, ".jsonl").unwrap();

        assert_eq!(found.files.len(), 1, "found {found:?}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_skipped_and_the_rest_is_still_read() {
        use std::os::unix::fs::PermissionsExt;

        let root = ScratchDir::new("unreadable-folder");
        let open = root.join("open");
        let locked = root.join("locked");
        fs::create_dir_all(&open).unwrap();
        fs::create_dir_all(&locked).unwrap();
        fs::write(open.join("a.jsonl"), "").unwrap();
        fs::write(locked.join("b.jsonl"), "").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let found = files_with_suffix(&root, ".jsonl");

        // Restore access so the scratch folder can be removed, whatever the result.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let found = found.unwrap();
        assert_eq!(
            found.files.len(),
            1,
            "the readable folder's file is still found: {found:?}"
        );
        assert_eq!(
            found.unreadable, 1,
            "the locked folder is counted as unreadable"
        );
    }

    #[test]
    fn an_unreadable_entry_is_counted_and_the_others_are_sorted() {
        let entries = vec![
            Ok(PathBuf::from("/nowhere/a.jsonl")),
            Err(io::Error::other("entry vanished")),
            Ok(PathBuf::from("/nowhere/notes.txt")),
        ];
        let mut found = Found::default();
        let mut pending = Vec::new();

        sort_entries(entries.into_iter(), ".jsonl", &mut found, &mut pending);

        assert_eq!(
            found.files,
            [PathBuf::from("/nowhere/a.jsonl")],
            "only the matching file is kept"
        );
        assert_eq!(
            found.unreadable, 1,
            "the entry that could not be read is counted"
        );
        assert!(pending.is_empty(), "no subfolder is queued");
    }

    #[test]
    fn missing_root_is_an_error_that_names_the_root() {
        let err = files_with_suffix(Path::new("/nonexistent-droid-usage-root"), ".jsonl")
            .expect_err("a missing root folder is reported as an error");
        assert!(
            err.to_string().contains("/nonexistent-droid-usage-root"),
            "the error names the root folder: {err}"
        );
    }
}
