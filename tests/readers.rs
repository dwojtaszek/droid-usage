//! Reader tests against real folders and databases, through the public API.

pub mod common;

use std::fs;
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use common::ScratchDir;
use droid_usage::prices::{self, PriceBook};
use droid_usage::{agy, droid, multipliers, opencode};
use rusqlite::{Connection, params};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn droid_reads_sessions_and_skips_settings_without_a_transcript() {
    let root = ScratchDir::new("droid");
    let project = root.join("-home-dom-project");
    write(
        &project.join("s1.settings.json"),
        r#"{"model":"glm-5.3","tokenUsage":{"inputTokens":10,"outputTokens":5,"cacheReadTokens":2,"cacheCreationTokens":1}}"#,
    );
    write(
        &project.join("s1.jsonl"),
        concat!(
            r#"{"type":"session_start","cwd":"/work/app"}"#,
            "\n",
            r#"{"type":"message","timestamp":"2026-10-05T15:04:35Z"}"#,
            "\n",
        ),
    );
    write(&project.join("orphan.settings.json"), "{}");

    let read = droid::read_sessions(&root).unwrap();

    assert_eq!(
        read.sessions.len(),
        1,
        "only the session that has a transcript is kept as a session"
    );
    assert_eq!(
        read.skipped, 1,
        "the settings file without a transcript is counted as skipped"
    );
    let session = &read.sessions[0];
    assert_eq!(
        session.project.as_deref(),
        Some("/work/app"),
        "the project comes from the session_start cwd in the transcript"
    );
    assert_eq!(
        session.model.as_deref(),
        Some("glm-5.3"),
        "the model comes from the settings file of the session"
    );
    assert_eq!(
        session.tokens.total(),
        18,
        "the total sums input output and both cache token counts"
    );
}

#[test]
fn droid_missing_root_is_an_error() {
    assert!(
        droid::read_sessions(Path::new("/nonexistent-droid-usage-root")).is_err(),
        "a missing droid root folder is reported as an error"
    );
}

#[test]
fn agy_reads_transcripts_with_models_from_their_databases() {
    let root = ScratchDir::new("agy");

    // Conversation with a transcript and a metadata database that names the model.
    write(
        &root.join("brain/conv-1/.system_generated/logs/transcript.jsonl"),
        r#"{"created_at":"2026-10-05T15:04:35Z","input_tokens":7,"output_tokens":3,"cache_read_tokens":2}"#,
    );
    let db = root.join("conversations/conv-1.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE gen_metadata (idx INTEGER, data BLOB);")
        .unwrap();
    assert_eq!(
        conn.execute(
            "INSERT INTO gen_metadata VALUES (?1, ?2)",
            params![0, b"\x01gemini-3.7-flash-tiered".as_slice()],
        )
        .unwrap(),
        1,
        "the model metadata row is inserted into the conversation database"
    );
    drop(conn);

    // Transcript with no database: the session is kept, with no model.
    write(
        &root.join("brain/conv-2/.system_generated/logs/transcript.jsonl"),
        r#"{"created_at":"2026-10-06T10:00:00Z","input_tokens":1,"output_tokens":1,"cache_read_tokens":0}"#,
    );

    // Conversation folder with no transcript: nothing to count, not an error.
    fs::create_dir_all(root.join("brain/empty-conv/.system_generated/logs")).unwrap();

    // Transcript with no timestamps: skipped.
    write(
        &root.join("brain/no-time/.system_generated/logs/transcript.jsonl"),
        r#"{"input_tokens":5}"#,
    );

    let read = agy::read_sessions(&root).unwrap();

    assert_eq!(
        read.sessions.len(),
        2,
        "both conversations that have a transcript are kept as sessions"
    );
    assert_eq!(
        read.skipped, 1,
        "the transcript without timestamps is counted as skipped"
    );
    let with_model = read
        .sessions
        .iter()
        .find(|s| s.model.is_some())
        .expect("conv-1 has a model");
    assert_eq!(
        with_model.model.as_deref(),
        Some("gemini-3.7-flash-tiered"),
        "the model name is read from the conversation metadata database"
    );
    assert_eq!(
        with_model.tokens.total(),
        12,
        "the conversation total sums input output and cache read tokens"
    );
    let without_model = read
        .sessions
        .iter()
        .find(|s| s.model.is_none())
        .expect("conv-2 has no model");
    assert_eq!(
        without_model.tokens.total(),
        2,
        "a conversation without a database still totals its transcript tokens"
    );
}

#[cfg(unix)]
#[test]
fn agy_transcript_that_cannot_be_opened_is_skipped_not_missing() {
    let root = ScratchDir::new("agy-loop");
    let transcript = root.join("brain/loop/.system_generated/logs/transcript.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    // A symlink that points at itself fails to open with a non-NotFound error.
    std::os::unix::fs::symlink(&transcript, &transcript).unwrap();

    let read = agy::read_sessions(&root).unwrap();

    assert_eq!(
        read.sessions,
        Vec::new(),
        "an unopenable transcript produces no session"
    );
    assert_eq!(
        read.skipped, 1,
        "an unopenable transcript is counted as skipped not as missing"
    );
}

#[test]
fn opencode_reads_a_database_file() {
    let root = ScratchDir::new("opencode");
    let db = root.join("opencode.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE session_v2 (
            directory TEXT, model TEXT, time_created INTEGER,
            tokens_input INTEGER, tokens_output INTEGER,
            tokens_cache_read INTEGER, tokens_cache_write INTEGER,
            cost REAL
        );
        INSERT INTO session_v2 VALUES
            ('/w/z', '{"id":"deepseek-v4","providerID":"opencode"}', 1792259191988, 4, 6, 8, 0, 0.25);
        "#,
    )
    .unwrap();
    drop(conn);

    let read = opencode::read_sessions(&db).unwrap();

    assert_eq!(
        read.sessions.len(),
        1,
        "the opencode database yields exactly one session"
    );
    assert_eq!(
        read.sessions[0].model.as_deref(),
        Some("deepseek-v4"),
        "the model id is taken from the model json field of the row"
    );
    assert_eq!(
        read.sessions[0].tokens.total(),
        18,
        "the total sums input output and cache read tokens of the row"
    );
}

#[test]
fn opencode_missing_database_is_an_error_and_is_not_created() {
    let missing = Path::new("/nonexistent-droid-usage/opencode.db");
    assert!(
        opencode::read_sessions(missing).is_err(),
        "a missing opencode database is reported as an error"
    );
    assert!(
        !missing.exists(),
        "reading a missing opencode database does not create the file"
    );
}

/// A one-model price table with the given input price per token.
fn price_table(input: &str) -> String {
    format!(
        r#"{{"zai/glm-5.3": {{"mode": "chat", "input_cost_per_token": {input},
            "output_cost_per_token": 4e-6, "cache_read_input_token_cost": 1e-7,
            "cache_creation_input_token_cost": 1.25e-6}}}}"#
    )
}

#[test]
fn droid_sessions_are_priced_with_the_snapshot_in_effect_when_they_started() {
    let root = ScratchDir::new("priced");
    let project = root.join("-home-dom-project");
    write(
        &project.join("s1.settings.json"),
        r#"{"model":"glm-5.3","tokenUsage":{"inputTokens":10,"outputTokens":5,"cacheReadTokens":2,"cacheCreationTokens":1}}"#,
    );
    write(
        &project.join("s1.jsonl"),
        concat!(
            r#"{"type":"session_start","cwd":"/work/app"}"#,
            "\n",
            r#"{"type":"message","timestamp":"2026-10-05T15:04:35Z"}"#,
            "\n",
        ),
    );

    // The session starts on Oct 5, after the Oct 1 prices and multipliers and before the Oct 10 changes.
    let prices_dir = root.join("prices");
    let multipliers_dir = root.join("multipliers");
    let early: DateTime<Utc> = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    let late: DateTime<Utc> = Utc.with_ymd_and_hms(2026, 10, 10, 0, 0, 0).unwrap();
    let page = |multiplier: &str| format!("| GLM-5.3 | `glm-5.3` | {multiplier}× | `high` |");
    let _ = prices::save(
        &prices_dir,
        &prices::parse_table(&price_table("1e-6"), early).unwrap(),
    )
    .unwrap();
    let _ = prices::save(
        &prices_dir,
        &prices::parse_table(&price_table("2e-6"), late).unwrap(),
    )
    .unwrap();
    let _ = multipliers::save(
        &multipliers_dir,
        &multipliers::parse_page(&page("2"), early).unwrap(),
    )
    .unwrap();
    let _ = multipliers::save(
        &multipliers_dir,
        &multipliers::parse_page(&page("3"), late).unwrap(),
    )
    .unwrap();

    let mut sessions = droid::read_sessions(&root).unwrap().sessions;
    let book: PriceBook = prices::load(&prices_dir).unwrap();
    let multiplier_book = multipliers::load(&multipliers_dir).unwrap();
    prices::price_sessions(&mut sessions, &book);
    multipliers::price_droid_sessions(&mut sessions, &book, &multiplier_book);

    // 10 input at 1e-6, 5 output at 4e-6, 2 cache reads at 1e-7, 1 cache write at 1.25e-6, times the early multiplier of 2.
    let expected = (10.0 * 1e-6 + 5.0 * 4e-6 + 2.0 * 1e-7 + 1.25e-6) * 2.0;
    let cost = sessions[0].cost_usd.unwrap();
    assert!((cost - expected).abs() < 1e-12, "cost was {cost}");
}
