//! Dated JSON snapshot persistence and check-time tracking.

use std::fs;
use std::io;
use std::path::Path;

use jiff::SignedDuration;
use jiff::Timestamp;
use serde_json::Value;

use crate::model::parse_timestamp;

/// How long a check stays fresh before the report downloads the table again.
const MAX_AGE_HOURS: i64 = 24;

/// File that records when the tables were last checked, kept beside the price snapshots.
const CHECKED_FILE: &str = "last-checked";

/// A book read from a snapshot folder, plus the names of the `.json` files that could not be used.
#[derive(Debug)]
pub struct Loaded<B> {
    /// The book built from every usable snapshot.
    pub book: B,
    /// File names of `.json` files that could not be read or parsed. They were ignored.
    pub skipped: Vec<String>,
}

/// What saving a snapshot did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Every entry matched the newest snapshot, so nothing was saved.
    Unchanged,
    /// At least one entry changed, so a new snapshot was saved.
    Updated,
}

/// Reads every valid snapshot in `dir` and builds a book via `build`. Missing folders produce an empty book.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be read.
pub(crate) fn read_book<S, B>(
    dir: &Path,
    parse: impl Fn(&Value) -> Option<S>,
    build: impl FnOnce(Vec<S>) -> B,
) -> io::Result<Loaded<B>> {
    let (snapshots, skipped) = read_snapshots(dir, parse)?;
    Ok(Loaded {
        book: build(snapshots),
        skipped,
    })
}

/// Reads every JSON file in `dir` that `parse` accepts, and returns the names of those it skipped. A missing folder holds none.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be read.
pub(crate) fn read_snapshots<T>(
    dir: &Path,
    parse: impl Fn(&Value) -> Option<T>,
) -> io::Result<(Vec<T>, Vec<String>)> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(err) => return Err(err),
    };
    let mut snapshots = Vec::new();
    let mut skipped = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let parsed = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| parse(&value));
        match parsed {
            Some(snapshot) => snapshots.push(snapshot),
            None => skipped.push(
                path.file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            ),
        }
    }
    skipped.sort();
    Ok((snapshots, skipped))
}

/// Saves a snapshot in `dir` when it differs from the newest saved snapshot.
///
/// # Errors
///
/// Returns an error when `dir` cannot be read or written.
pub(crate) fn save_if_changed<S>(
    dir: &Path,
    fetched: Timestamp,
    parse: impl Fn(&Value) -> Option<S>,
    sort_key: impl Fn(&S) -> Timestamp,
    matches_newest: impl FnOnce(&S) -> bool,
    to_json: impl FnOnce() -> Value,
) -> io::Result<Refresh> {
    let (snapshots, _) = read_snapshots(dir, parse)?;
    if snapshots
        .iter()
        .max_by_key(|snapshot| sort_key(snapshot))
        .is_some_and(matches_newest)
    {
        return Ok(Refresh::Unchanged);
    }
    write_snapshot(dir, fetched, &to_json())
}

/// The snapshot in effect at `at`: the latest one fetched by then, or the earliest when `at` comes before all of them. `snapshots` must be sorted oldest first.
pub(crate) fn snapshot_in_effect<S>(
    snapshots: &[S],
    at: Timestamp,
    fetched: impl Fn(&S) -> Timestamp,
) -> Option<&S> {
    snapshots
        .iter()
        .rev()
        .find(|&snapshot| fetched(snapshot) <= at)
        .or_else(|| snapshots.first())
}

/// Writes `json` into `dir` as a new snapshot named for `fetched`, creating `dir` first if needed. The snapshot appears in one step, so a crash never leaves a partial `.json` file for readers to parse.
///
/// # Errors
///
/// Returns an error when `dir` cannot be created or written.
pub(crate) fn write_snapshot(dir: &Path, fetched: Timestamp, json: &Value) -> io::Result<Refresh> {
    fs::create_dir_all(dir)?;
    let name = snapshot_file_name(fetched);
    // The temp name ends in `.tmp`, so `read_snapshots` never reads it.
    let temp = dir.join(format!(".{name}.tmp"));
    let result =
        write_synced(&temp, &json.to_string()).and_then(|()| fs::rename(&temp, dir.join(&name)));
    if result.is_err() {
        // Best effort: a leftover temp file is ignored by readers.
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(Refresh::Updated)
}

fn write_synced(path: &Path, text: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    io::Write::write_all(&mut file, text.as_bytes())?;
    file.sync_all()
}

/// Removes the snapshot that `write_snapshot` wrote for `fetched`.
///
/// # Errors
///
/// Returns an error when the snapshot cannot be removed.
pub(crate) fn remove_snapshot(dir: &Path, fetched: Timestamp) -> io::Result<()> {
    fs::remove_file(dir.join(snapshot_file_name(fetched)))
}

fn snapshot_file_name(fetched: Timestamp) -> String {
    format!("{}.json", fetched.strftime("%Y%m%dT%H%M%SZ"))
}

/// When the tables were last checked, or `None` if they never were.
#[must_use]
pub fn last_checked(dir: &Path) -> Option<Timestamp> {
    let text = fs::read_to_string(dir.join(CHECKED_FILE)).ok()?;
    parse_timestamp(text.trim())
}

/// True when the tables have not been checked within the last day.
#[must_use]
pub fn is_stale(dir: &Path, now: Timestamp) -> bool {
    last_checked(dir).is_none_or(|checked| {
        now.duration_since(checked) > SignedDuration::from_hours(MAX_AGE_HOURS)
    })
}

/// Records `now` as the time the tables were last checked.
///
/// # Errors
///
/// Returns an error when `dir` cannot be written.
pub fn record_check(dir: &Path, now: Timestamp) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join(CHECKED_FILE),
        now.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use jiff::civil::Date;

    use serde_json::json;

    use super::*;
    use crate::testing::ScratchDir;

    fn at(day: i8) -> Timestamp {
        Date::new(2026, 10, day)
            .unwrap()
            .at(12, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
    }

    #[test]
    fn snapshot_in_effect_picks_latest_before_or_earliest() {
        let items = [at(5), at(10)];
        assert_eq!(snapshot_in_effect(&items, at(1), |&t| t), Some(&at(5)));
        assert_eq!(snapshot_in_effect(&items, at(7), |&t| t), Some(&at(5)));
        assert_eq!(snapshot_in_effect(&items, at(10), |&t| t), Some(&at(10)));
        assert_eq!(snapshot_in_effect(&items, at(15), |&t| t), Some(&at(10)));
        assert_eq!(snapshot_in_effect::<Timestamp>(&[], at(10), |&t| t), None);
    }

    #[test]
    fn stale_after_twenty_four_hours() {
        let dir = ScratchDir::new("snapshots-stale");
        assert!(is_stale(&dir, at(10)));
        record_check(&dir, at(10)).unwrap();
        assert_eq!(last_checked(&dir), Some(at(10)));
        let day = SignedDuration::from_hours(24);
        assert!(!is_stale(&dir, at(10) + day));
        assert!(is_stale(&dir, at(10) + day + SignedDuration::from_secs(1)));
    }

    #[test]
    fn write_and_remove_snapshot_round_trip() {
        let dir = ScratchDir::new("snapshots-write-remove");
        let saved = write_snapshot(&dir, at(10), &json!({"v": 1})).unwrap();
        assert_eq!(saved, Refresh::Updated);
        let (items, skipped) = read_snapshots(&dir, |v| v["v"].as_u64()).unwrap();
        assert_eq!(items, [1]);
        assert_eq!(skipped, Vec::<String>::new());
        remove_snapshot(&dir, at(10)).unwrap();
        let (items, _) = read_snapshots(&dir, |v| v["v"].as_u64()).unwrap();
        assert_eq!(items, Vec::<u64>::new());
    }

    #[test]
    fn save_if_changed_compares_against_the_newest_snapshot() {
        let dir = ScratchDir::new("snapshots-save-if-changed");
        let parse = |v: &Value| Some((parse_timestamp(v["t"].as_str()?)?, v["v"].as_u64()?));
        let _ = write_snapshot(&dir, at(5), &json!({"t": "2026-10-05T12:00:00Z", "v": 1})).unwrap();
        let _ =
            write_snapshot(&dir, at(10), &json!({"t": "2026-10-10T12:00:00Z", "v": 2})).unwrap();

        let same_as_newest = save_if_changed(
            &dir,
            at(12),
            parse,
            |&(t, _)| t,
            |&(_, v)| v == 2,
            || json!({"t": "2026-10-12T12:00:00Z", "v": 2}),
        )
        .unwrap();
        assert_eq!(same_as_newest, Refresh::Unchanged);

        let reverted_to_older = save_if_changed(
            &dir,
            at(15),
            parse,
            |&(t, _)| t,
            |&(_, v)| v == 1,
            || json!({"t": "2026-10-15T12:00:00Z", "v": 1}),
        )
        .unwrap();
        assert_eq!(reverted_to_older, Refresh::Updated);
    }
}
