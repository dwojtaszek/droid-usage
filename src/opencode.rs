//! Reader for `OpenCode`'s `SQLite` database.

use std::path::Path;

use jiff::Timestamp;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::model::{Read, Session, Source, Tokens};

/// Reads every `OpenCode` session from its `SQLite` database, without modifying it.
///
/// # Errors
///
/// Returns an error when the database cannot be opened or the query fails.
pub fn read_sessions(db: &Path) -> rusqlite::Result<Read> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    read_from(&conn)
}

/// Reads sessions from an open connection to a database with the `session_v2` table of `OpenCode`.
///
/// # Errors
///
/// Returns an error when the query fails, for example when the table is missing.
pub fn read_from(conn: &Connection) -> rusqlite::Result<Read> {
    let mut stmt = conn.prepare(
        "SELECT directory, model, time_created, tokens_input, tokens_output, \
         tokens_cache_read, tokens_cache_write, cost FROM session_v2",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, SqlValue>(2)?,
            Tokens {
                input: count_column(row, 3)?,
                output: count_column(row, 4)?,
                cache_read: count_column(row, 5)?,
                cache_write: count_column(row, 6)?,
            },
            // A negative or non-finite cost is not trusted, so the session is priced from the table.
            row.get::<_, Option<f64>>(7)?
                .filter(|cost| cost.is_finite() && *cost >= 0.0),
        ))
    })?;

    let mut read = Read::default();
    for row in rows {
        let (project, model, created, tokens, cost) = row?;
        match started_at(&created) {
            Some(started) => read.sessions.push(Session {
                source: Source::OpenCode,
                started,
                project,
                model: model.as_deref().and_then(model_id),
                tokens,
                cost_usd: cost,
            }),
            None => read.skipped += 1,
        }
    }
    Ok(read)
}

/// NULL, negative, or missing counts are treated as zero.
fn count_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: Option<i64> = row.get(index)?;
    Ok(value.and_then(|v| u64::try_from(v).ok()).unwrap_or(0))
}

/// `time_created` holds epoch milliseconds, but its column type is TEXT, so SQLite may store them as text.
fn started_at(value: &SqlValue) -> Option<Timestamp> {
    let millis = match value {
        SqlValue::Integer(millis) => *millis,
        SqlValue::Text(text) => text.parse().ok()?,
        _ => return None,
    };
    Timestamp::from_millisecond(millis).ok()
}

/// The model id from a stored model value. The value is JSON with an `id` field, or a plain name in older rows.
fn model_id(stored: &str) -> Option<String> {
    serde_json::from_str::<Value>(stored).map_or_else(
        |_| Some(stored.to_owned()),
        |value| value["id"].as_str().map(str::to_owned),
    )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{any, prop_assert_eq, proptest};

    use super::*;

    /// Mirrors the real table. `time_created` is TEXT, so integers stored in it become text.
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE session_v2 (
                directory TEXT, model TEXT, time_created TEXT,
                tokens_input INTEGER, tokens_output INTEGER,
                tokens_cache_read INTEGER, tokens_cache_write INTEGER,
                cost REAL
            );
            INSERT INTO session_v2 VALUES
                ('/w/a', '{"id":"m1","providerID":"p"}', 1792259191988, 10, 20, 30, 40, 0.5),
                (NULL, 'plain-model', 1792259191988, NULL, NULL, NULL, NULL, NULL),
                ('/w/b', NULL, 'not-a-number', 1, 1, 1, 1, 0.0),
                ('/w/c', NULL, 1792259191988, -5, 0, 0, 0, 0.0);
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn reads_sessions_and_counts_unusable_rows() {
        let read = read_from(&fixture()).unwrap();
        assert_eq!(
            read.sessions.len(),
            3,
            "the three usable session rows are read back from the database"
        );
        assert_eq!(
            read.skipped, 1,
            "exactly one unusable session row is counted as skipped"
        );

        let first = &read.sessions[0];
        assert_eq!(
            first.project.as_deref(),
            Some("/w/a"),
            "the first session keeps the directory it was recorded with"
        );
        assert_eq!(
            first.model.as_deref(),
            Some("m1"),
            "the first session keeps the model id from its model json"
        );
        assert_eq!(
            first.started.as_millisecond(),
            1_792_259_191_988,
            "the first session start time is the recorded millisecond timestamp"
        );
        assert_eq!(
            first.tokens,
            Tokens {
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40
            },
            "the first session token counts match the recorded input output and cache values"
        );
    }

    #[test]
    fn null_and_negative_counts_are_zero_and_plain_model_names_are_kept() {
        let read = read_from(&fixture()).unwrap();

        let plain = &read.sessions[1];
        assert_eq!(
            plain.project, None,
            "a session with no directory has no project name"
        );
        assert_eq!(
            plain.model.as_deref(),
            Some("plain-model"),
            "a plain model name is kept as the model unchanged"
        );
        assert_eq!(
            plain.tokens,
            Tokens::default(),
            "a session with null token counts has zero tokens"
        );

        assert_eq!(
            read.sessions[2].tokens.input, 0,
            "a negative token count is read as zero"
        );
    }

    #[test]
    fn recorded_costs_are_kept_and_missing_costs_stay_empty() {
        let read = read_from(&fixture()).unwrap();
        assert_eq!(
            read.sessions[0].cost_usd,
            Some(0.5),
            "a recorded session cost is kept as given"
        );
        assert_eq!(
            read.sessions[1].cost_usd, None,
            "a session with no recorded cost has no cost"
        );
        assert_eq!(
            read.sessions[2].cost_usd,
            Some(0.0),
            "a recorded zero cost is kept as zero and not dropped"
        );
    }

    #[test]
    fn a_recorded_cost_must_be_finite_and_not_negative() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r"
            CREATE TABLE session_v2 (
                directory TEXT, model TEXT, time_created TEXT,
                tokens_input INTEGER, tokens_output INTEGER,
                tokens_cache_read INTEGER, tokens_cache_write INTEGER,
                cost REAL
            );
            INSERT INTO session_v2 VALUES
                ('/w/neg', NULL, 1792259191988, 1, 0, 0, 0, -1.5),
                ('/w/inf', NULL, 1792259191988, 1, 0, 0, 0, 1e999),
                ('/w/zero', NULL, 1792259191988, 1, 0, 0, 0, 0.0);
            ",
        )
        .unwrap();
        let read = read_from(&conn).unwrap();
        let costs: Vec<Option<f64>> = read.sessions.iter().map(|s| s.cost_usd).collect();
        assert_eq!(
            costs,
            [None, None, Some(0.0)],
            "negative and infinite recorded costs are dropped while zero is kept"
        );
    }

    #[test]
    fn model_id_reads_the_id_field_only() {
        assert_eq!(
            model_id(r#"{"id":"m"}"#).as_deref(),
            Some("m"),
            "the model id is read from the id field of the model json"
        );
        assert_eq!(
            model_id(r#"{"other":1}"#),
            None,
            "a model json without an id field gives no model id"
        );
    }

    #[test]
    fn opencode_integer_timestamps_are_read_when_the_column_keeps_integers() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r"
            CREATE TABLE session_v2 (
                directory TEXT, model TEXT, time_created,
                tokens_input INTEGER, tokens_output INTEGER,
                tokens_cache_read INTEGER, tokens_cache_write INTEGER,
                cost REAL
            );
            INSERT INTO session_v2 VALUES ('/w', NULL, 1792259191988, 1, 0, 0, 0, NULL);
            ",
        )
        .unwrap();

        let read = read_from(&conn).unwrap();
        assert_eq!(
            read.sessions.len(),
            1,
            "the session row is read when timestamps are stored as integers"
        );
        assert_eq!(
            read.sessions[0].started.as_millisecond(),
            1_792_259_191_988,
            "an integer timestamp is read as the same millisecond time"
        );
    }

    proptest! {
        #[test]
        fn stored_model_text_never_panics(text in ".*") {
            let _ = model_id(&text);
        }

        #[test]
        fn stored_time_text_never_panics(text in ".*") {
            let _ = started_at(&SqlValue::Text(text));
        }

        #[test]
        fn integer_time_stored_as_text_reads_as_epoch_millis(millis in any::<i64>()) {
            prop_assert_eq!(
                started_at(&SqlValue::Text(millis.to_string())),
                Timestamp::from_millisecond(millis).ok()
            );
        }
    }
}
