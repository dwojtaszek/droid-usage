//! Reader for agy (Antigravity CLI) conversations.

use std::fs::File;
use std::io::{self, BufRead, BufReader, ErrorKind};
use std::ops::ControlFlow;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};

use crate::files::{LineError, for_each_json_line};
use crate::model::{Read, Session, Source, Tokens, parse_timestamp};

/// Model name prefixes to look for inside agy's binary metadata (mirrored as `AGY_MODEL_PREFIXES` in `tests/golden/anonymize.py`).
// shortcut: model names are found by prefix in a binary blob, so other providers show as unknown and a conversation that used several models reports only the first; upgrade when agy stores the model id as a plain field.
const MODEL_PREFIXES: [&str; 3] = ["gemini-", "claude-", "gpt-"];

/// Reads every agy conversation under `root` (the `antigravity-cli` folder). Usage comes from each conversation's transcript, and the model from its database.
///
/// # Errors
///
/// Returns an error when the `brain` folder under `root` cannot be read.
pub fn read_sessions(root: &Path) -> io::Result<Read> {
    let mut read = Read::default();
    for entry in std::fs::read_dir(root.join("brain"))? {
        let dir = entry?.path();
        let Some(id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let transcript = dir.join(".system_generated/logs/transcript.jsonl");
        let file = match File::open(&transcript) {
            Ok(file) => file,
            // Some conversations never wrote a transcript; there is nothing to count.
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(_) => {
                read.skipped += 1;
                continue;
            }
        };
        let Some((started, tokens)) = parse_transcript(BufReader::new(file)) else {
            read.skipped += 1;
            continue;
        };
        let db = root.join("conversations").join(format!("{id}.db"));
        let model = if matches!(db.try_exists(), Ok(false)) {
            None
        } else if let Ok(model) = model_from_db(&db) {
            model
        } else {
            read.models_unreadable += 1;
            None
        };
        read.sessions.push(Session {
            source: Source::Agy,
            started,
            project: None,
            model,
            tokens,
            cost_usd: None,
        });
    }
    Ok(read)
}

/// Earliest timestamp and summed usage from one transcript. Lines that do not parse are skipped.
pub fn parse_transcript(reader: impl BufRead) -> Option<(DateTime<Utc>, Tokens)> {
    let mut started: Option<DateTime<Utc>> = None;
    let mut tokens = Tokens::default();
    let _ = for_each_json_line(reader, LineError::Skip, |value| {
        if let Some(at) = value["created_at"].as_str().and_then(parse_timestamp) {
            started = Some(started.map_or(at, |first| first.min(at)));
        }
        tokens.add(&Tokens {
            input: value["input_tokens"].as_u64().unwrap_or(0),
            output: value["output_tokens"].as_u64().unwrap_or(0),
            cache_read: value["cache_read_tokens"].as_u64().unwrap_or(0),
            cache_write: 0,
        });
        ControlFlow::Continue(())
    });
    started.map(|started| (started, tokens))
}

/// The first model name found in a conversation's metadata. Errors when the database cannot be opened or read.
fn model_from_db(path: &Path) -> rusqlite::Result<Option<String>> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    model_from_conn(&conn)
}

fn model_from_conn(conn: &Connection) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT data FROM gen_metadata ORDER BY idx")?;
    let blobs = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    for blob in blobs {
        if let Some(name) = first_model_name(&blob?) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Splits the blob on anything that cannot be part of a model id and returns the first token with a known prefix.
pub fn first_model_name(blob: &[u8]) -> Option<String> {
    blob.split(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
        .filter_map(|token| std::str::from_utf8(token).ok())
        .find(|token| {
            MODEL_PREFIXES
                .iter()
                .any(|prefix| token.starts_with(prefix))
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use proptest::prelude::{any, prop, prop_assert, proptest};

    use super::*;

    #[test]
    fn transcript_sums_usage_and_takes_the_earliest_time() {
        let text = concat!(
            r#"{"created_at":"2026-10-05T15:05:00Z","input_tokens":5,"output_tokens":2,"cache_read_tokens":1}"#,
            "\n",
            "not json\n",
            r#"{"created_at":"2026-10-05T15:04:35Z","input_tokens":7,"output_tokens":3,"cache_read_tokens":0}"#,
            "\n",
            r#"{"type":"USER_INPUT"}"#,
            "\n",
        );
        let (started, tokens) = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(
            started.to_rfc3339(),
            "2026-10-05T15:04:35+00:00",
            "the start time is the earliest transcript timestamp"
        );
        assert_eq!(
            tokens,
            Tokens {
                input: 12,
                output: 5,
                cache_read: 1,
                cache_write: 0
            },
            "usage is summed over all transcript lines"
        );
    }

    /// A conversation with a transcript, and its database content if `db` is given.
    fn conversation_root(name: &str, db: Option<&str>) -> crate::testing::ScratchDir {
        let root = crate::testing::ScratchDir::new(name);
        let logs = root.join("brain/abc/.system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("transcript.jsonl"),
            r#"{"created_at":"2026-10-05T15:04:35Z","input_tokens":5}"#,
        )
        .unwrap();
        if let Some(db) = db {
            std::fs::create_dir_all(root.join("conversations")).unwrap();
            std::fs::write(root.join("conversations/abc.db"), db).unwrap();
        }
        root
    }

    #[test]
    fn a_conversation_without_a_database_has_no_model_and_no_warning() {
        let root = conversation_root("agy-no-db", None);

        let read = read_sessions(&root).unwrap();

        assert_eq!(read.sessions.len(), 1, "the conversation is still counted");
        assert_eq!(read.sessions[0].model, None, "no database means no model");
        assert_eq!(
            read.models_unreadable, 0,
            "a missing database is not a read failure"
        );
    }

    #[test]
    fn a_conversation_whose_database_cannot_be_read_is_counted_as_such() {
        let root = conversation_root("agy-bad-db", Some("not a database"));

        let read = read_sessions(&root).unwrap();

        assert_eq!(read.sessions.len(), 1, "the conversation is still counted");
        assert_eq!(
            read.sessions[0].model, None,
            "an unreadable database gives no model"
        );
        assert_eq!(
            read.models_unreadable, 1,
            "the unreadable model is counted so the report can warn about it"
        );
    }

    #[test]
    fn transcript_usage_stops_at_the_maximum_instead_of_overflowing() {
        let line = r#"{"created_at":"2026-10-05T15:04:35Z","input_tokens":18446744073709551615}"#;
        let text = format!("{line}\n{line}\n");
        let (_, tokens) = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(
            tokens.input,
            u64::MAX,
            "usage summed past u64::MAX stops at u64::MAX"
        );
    }

    #[test]
    fn earliest_time_is_compared_as_an_instant_not_as_text() {
        // 10:00 at UTC+2 is 08:00 UTC, earlier than 08:30 UTC. As text, "10:00" sorts after "08:30".
        let text = concat!(
            r#"{"created_at":"2026-10-05T10:00:00+02:00","input_tokens":1}"#,
            "\n",
            r#"{"created_at":"2026-10-05T08:30:00Z","input_tokens":1}"#,
            "\n",
        );
        let (started, _) = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(
            started.to_rfc3339(),
            "2026-10-05T08:00:00+00:00",
            "the earliest time is picked by instant not by text"
        );
    }

    #[test]
    fn transcript_without_timestamps_is_none() {
        assert!(
            parse_transcript(Cursor::new(r#"{"input_tokens":5}"#)).is_none(),
            "a transcript without timestamps gives no result"
        );
    }

    #[test]
    fn model_name_is_found_by_prefix() {
        let blob = b"\x01\x17gemini-3.7-flash-tiered\x0bused_claude\x00";
        assert_eq!(
            first_model_name(blob).as_deref(),
            Some("gemini-3.7-flash-tiered"),
            "the model name is found from its prefix in the blob"
        );
        assert_eq!(
            first_model_name(b"used_claude_conservative only"),
            None,
            "no model name is returned without the prefix"
        );
    }

    #[test]
    fn model_comes_from_the_lowest_metadata_index() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE gen_metadata (idx INTEGER, data BLOB);")
            .unwrap();
        assert_eq!(
            conn.execute(
                "INSERT INTO gen_metadata VALUES (?1, ?2)",
                rusqlite::params![1, b"gpt-6-sol".as_slice()],
            )
            .unwrap(),
            1,
            "the metadata insert changes exactly one row"
        );
        assert_eq!(
            conn.execute(
                "INSERT INTO gen_metadata VALUES (?1, ?2)",
                rusqlite::params![0, b"claude-fable-5".as_slice()],
            )
            .unwrap(),
            1,
            "the second metadata insert changes exactly one row"
        );
        assert_eq!(
            model_from_conn(&conn).unwrap().as_deref(),
            Some("claude-fable-5"),
            "the model comes from the row with the lowest index"
        );
    }

    proptest! {
        #[test]
        fn model_names_always_start_with_a_known_prefix(
            blob in prop::collection::vec(any::<u8>(), 0..256),
        ) {
            let _ = first_model_name(&blob);
            if let Some(name) = first_model_name(&blob) {
                prop_assert!(MODEL_PREFIXES.iter().any(|prefix| name.starts_with(prefix)));
            }
        }
    }
}
