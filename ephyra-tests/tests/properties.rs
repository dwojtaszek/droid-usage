//! Ephyra properties exercising `droid-usage` parsers, pricing, summary, and reporting.
#![allow(
    clippy::needless_pass_by_value,
    reason = "ephyra::test properties receive generated inputs by value"
)]

use std::collections::BTreeMap;
use std::io::Cursor;

use droid_usage::agy::{self, first_model_name};
use droid_usage::app::Mode;
use droid_usage::cli::{self, ArgError};
use droid_usage::codex;
use droid_usage::droid;
use droid_usage::model::{Session, Source, Tokens, retain_since};
use droid_usage::multipliers::{self, MultiplierBook, PageError, Snapshot as MultSnapshot};
use droid_usage::opencode;
use droid_usage::pi;
use droid_usage::prices::{self, PriceBook, PriceError, Rate, Snapshot as PriceSnapshot, cost_usd};
use droid_usage::pricing::price_sessions;
use droid_usage::report::{Colors, write_report};
use droid_usage::summary::weekly;
use droid_usage_ephyra::cli_droid::{
    CliArgCase, DroidHeaderLine, DroidSessionCase, MultiSnapshotCase, cli_arg_cases,
    droid_session_cases, multi_snapshot_cases,
};
use droid_usage_ephyra::droid_usage_config;
use droid_usage_ephyra::multipliers::{MultiplierPageCase, Toggle, multiplier_pages};
use droid_usage_ephyra::opencode_agy::{
    AgyBlob, AgyTranscript, OpenCodeTable, agy_blobs, agy_transcripts, opencode_tables,
};
use droid_usage_ephyra::prices::{PriceEntryKind, PriceTableCase, price_tables};
use droid_usage_ephyra::pricing_report::{SessionBatch, session_batches};
use droid_usage_ephyra::transcripts::{CodexStream, PiLine, PiStream, codex_streams, pi_streams};
use jiff::Timestamp;
use jiff::civil::Date;
use rusqlite::{Connection, params};
use unicode_width::UnicodeWidthStr;

fn at_ts(secs: i64) -> Timestamp {
    Timestamp::new(secs, 0).expect("valid timestamp")
}

#[ephyra::test(gen = multiplier_pages(), config = droid_usage_config())]
fn multipliers_valid_pages_parse_roundtrip_and_apply_promos(case: MultiplierPageCase) {
    let page = case.render();
    let snapshot = multipliers::parse_page(&page, case.fetched)
        .unwrap_or_else(|err| panic!("valid page rejected ({err}):\n{page}"));

    let expected_extra = usize::from(case.extra_unnoted_duplicate_name == Toggle::Yes);
    assert_eq!(snapshot.models.len(), case.rows.len() + expected_extra);

    let roundtrip = MultSnapshot::from_json(&snapshot.to_json());
    assert_eq!(roundtrip, Some(snapshot.clone()));

    let book = MultiplierBook::new(vec![snapshot]);
    for row in &case.rows {
        let during = Date::new(2025, 1, 1)
            .unwrap()
            .at(0, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp();
        if let Some((through, after)) = row.promo {
            let on_last_day = through
                .at(23, 59, 59, 0)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .unwrap()
                .timestamp();
            let after_day = through
                .tomorrow()
                .unwrap()
                .at(0, 0, 0, 0)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .unwrap()
                .timestamp();
            assert_eq!(book.multiplier_at(&row.id, on_last_day), Some(row.rate));
            assert_eq!(book.multiplier_at(&row.id, after_day), Some(after));
        } else {
            assert_eq!(book.multiplier_at(&row.id, during), Some(row.rate));
        }
    }
}

#[ephyra::test(gen = multiplier_pages(), config = droid_usage_config())]
fn multipliers_error_cases_match_page_error_contract(case: MultiplierPageCase) {
    let first = case.rows.first().expect("valid case has at least one row");
    let base = case.render();

    // Duplicate model ID must fail with DuplicateModel(first.id).
    let dup_id_page = format!(
        "{base}\n| Unique-Dup-Name | `{}` | 1.0× | `none` |",
        first.id
    );
    assert!(matches!(
        multipliers::parse_page(&dup_id_page, case.fetched),
        Err(PageError::DuplicateModel(ref id)) if id == &first.id
    ));

    // A promo note matching two rows with the same name must fail with PageError::Note.
    if let Some(promo_row) = case.rows.iter().find(|r| r.promo.is_some()) {
        let dup_name_page = format!(
            "{base}\n| {} | `extra-unique-id` | {}× | `none` |",
            promo_row.name, promo_row.rate
        );
        assert!(matches!(
            multipliers::parse_page(&dup_name_page, case.fetched),
            Err(PageError::Note(_))
        ));
    }
}

#[ephyra::test(gen = price_tables(), config = droid_usage_config())]
fn prices_parse_lookup_and_roundtrip_match_oracle(case: PriceTableCase) {
    let text = case.to_json_text();
    let snapshot = prices::parse_table(&text, case.fetched)
        .unwrap_or_else(|err| panic!("valid price table rejected ({err}): {text}"));

    assert_eq!(
        PriceSnapshot::from_json(&snapshot.to_json()),
        Some(snapshot.clone())
    );

    let mut expected_rates = BTreeMap::new();
    for (key, kind) in &case.entries {
        if let PriceEntryKind::Chat {
            input,
            output,
            cache_read,
            cache_write,
        }
        | PriceEntryKind::Responses {
            input,
            output,
            cache_read,
            cache_write,
        } = kind
        {
            let _ = expected_rates.insert(
                key.clone(),
                Rate {
                    input: *input,
                    output: *output,
                    cache_read: cache_read.unwrap_or(*input),
                    cache_write: cache_write.unwrap_or(*input),
                },
            );
        }
    }
    assert_eq!(snapshot.rates, expected_rates);

    let book = PriceBook::new(vec![snapshot]);
    let expected_bare = expected_rates.get(&case.query_bare).copied().or_else(|| {
        expected_rates.iter().find_map(|(k, v)| {
            k.split_once('/')
                .filter(|(_, n)| *n == case.query_bare)
                .map(|_| *v)
        })
    });
    assert_eq!(book.rate_at(&case.query_bare, case.fetched), expected_bare);
    assert_eq!(
        book.exact_rate_at(&case.query_bare, case.fetched),
        expected_rates.get(&case.query_bare).copied()
    );

    // Explicit provider tie-break check when bare key is absent and multi-level/sibling keys exist.
    if !expected_rates.contains_key("gpt-x")
        && let (Some(az), None) = (
            expected_rates.get("azure/gpt-x"),
            expected_rates.get("anthropic/gpt-x"),
        )
    {
        assert_eq!(book.rate_at("gpt-x", case.fetched), Some(*az));
    }
}

#[ephyra::test(gen = price_tables(), config = droid_usage_config())]
fn prices_negative_rate_rejects_whole_table(case: PriceTableCase) {
    let base = case.to_json_text();
    for (mode, field) in [
        ("chat", "input_cost_per_token"),
        ("chat", "output_cost_per_token"),
        ("chat", "cache_read_input_token_cost"),
        ("chat", "cache_creation_input_token_cost"),
        ("responses", "input_cost_per_token"),
        ("responses", "cache_creation_input_token_cost"),
    ] {
        let mut text = base.clone();
        let _ = text.pop();
        let extra = format!(
            r#","__bad":{{"mode":"{mode}","input_cost_per_token":1.0,"output_cost_per_token":1.0,"{field}":-0.1}}}}"#
        );
        text.push_str(&extra);
        assert!(
            matches!(
                prices::parse_table(&text, case.fetched),
                Err(PriceError::Shape(_))
            ),
            "accepted negative {mode}.{field}"
        );
    }
}

#[ephyra::test(gen = codex_streams(), config = droid_usage_config())]
fn codex_stream_matches_reference_oracle(stream: CodexStream) {
    let text = stream.render();
    let parsed = codex::parse_transcript(Cursor::new(&text))
        .unwrap_or_else(|| panic!("valid codex stream returned None:\n{text}"));
    let expected = stream.expected().expect("valid stream has started");
    assert_eq!(parsed.source, Source::Codex);
    assert_eq!(parsed.started, expected.started);
    assert_eq!(parsed.project, expected.project);
    assert_eq!(parsed.model, expected.model);
    assert_eq!(parsed.tokens, expected.tokens);
    assert_eq!(parsed.cost_usd, None);

    // Invalid UTF-8 line anywhere aborts and rejects the session.
    let mut bad_bytes = text.into_bytes();
    bad_bytes.extend_from_slice(b"\n\xff\n");
    assert!(codex::parse_transcript(Cursor::new(bad_bytes)).is_none());
}

#[ephyra::test(gen = pi_streams(), config = droid_usage_config())]
fn pi_stream_sums_usage_and_ignores_non_positive_costs(stream: PiStream) {
    let text = stream.render();
    let parsed = pi::parse_transcript(Cursor::new(&text))
        .unwrap_or_else(|| panic!("valid pi stream returned None:\n{text}"));

    let mut exp_started = None;
    let mut exp_project = None;
    let mut exp_model = None;
    let mut exp_tokens = Tokens::default();
    let mut exp_cost: Option<f64> = None;

    for line in &stream.lines {
        match line {
            PiLine::Session { ts_secs, cwd } if exp_started.is_none() => {
                exp_started = Some(at_ts(*ts_secs));
                exp_project.clone_from(cwd);
            }
            PiLine::ModelChange { model_id: Some(m) } => exp_model = Some(m.clone()),
            PiLine::Message {
                model,
                input,
                output,
                cache_read,
                cache_write,
                cost,
            } => {
                if let Some(m) = model {
                    exp_model = Some(m.clone());
                }
                exp_tokens.add(&Tokens {
                    input: *input,
                    output: *output,
                    cache_read: *cache_read,
                    cache_write: *cache_write,
                });
                if let Some(c) = cost.filter(|c| *c > 0.0) {
                    exp_cost = Some(exp_cost.unwrap_or(0.0) + c);
                }
            }
            PiLine::Aux {
                model,
                input,
                output,
                cost,
                ..
            } => {
                if let Some(m) = model {
                    exp_model = Some(m.clone());
                }
                exp_tokens.add(&Tokens {
                    input: *input,
                    output: *output,
                    cache_read: 0,
                    cache_write: 0,
                });
                if let Some(c) = cost.filter(|c| *c > 0.0) {
                    exp_cost = Some(exp_cost.unwrap_or(0.0) + c);
                }
            }
            _ => {}
        }
    }

    assert_eq!(parsed.source, Source::Pi);
    assert_eq!(parsed.started, exp_started.expect("session header"));
    assert_eq!(parsed.project, exp_project);
    assert_eq!(parsed.model, exp_model);
    assert_eq!(parsed.tokens, exp_tokens);
    match (parsed.cost_usd, exp_cost) {
        (Some(got), Some(want)) => assert!((got - want).abs() < 1e-12, "got {got}, want {want}"),
        (got, want) => assert_eq!(got, want),
    }

    // Invalid UTF-8 line aborts and rejects the Pi transcript.
    let mut bad_bytes = text.into_bytes();
    bad_bytes.extend_from_slice(b"\n\xff\n");
    assert!(pi::parse_transcript(Cursor::new(bad_bytes)).is_none());
}

#[ephyra::test(gen = opencode_tables(), config = droid_usage_config())]
fn opencode_read_from_handles_mixed_types_and_invalid_values(table: OpenCodeTable) {
    let conn = Connection::open_in_memory().expect("in-memory sqlite");
    conn.execute_batch(
        "CREATE TABLE session_v2 (
            directory TEXT, model TEXT, time_created,
            tokens_input INTEGER, tokens_output INTEGER,
            tokens_cache_read INTEGER, tokens_cache_write INTEGER,
            cost REAL
        );",
    )
    .expect("create table");

    let mut expected = Vec::new();
    let mut expected_skipped = 0usize;
    for row in &table.rows {
        let time_sql: rusqlite::types::Value = match &row.time_created {
            None => rusqlite::types::Value::Null,
            Some(Ok(ms)) if row.time_as_text => rusqlite::types::Value::Text(ms.to_string()),
            Some(Ok(ms)) => rusqlite::types::Value::Integer(*ms),
            Some(Err(s)) => rusqlite::types::Value::Text(s.clone()),
        };
        if let Some(Ok(ms)) = row.time_created
            && Timestamp::from_millisecond(ms).is_ok()
        {
            let model = row.model.as_deref().and_then(|s| match s {
                r#"{"providerID":"only"}"# => None,
                r#"{"id":"gpt-5.5","providerID":"openai"}"# => Some("gpt-5.5".to_owned()),
                other => Some(other.to_owned()),
            });
            let tokens = Tokens {
                input: row
                    .tokens_input
                    .and_then(|v| u64::try_from(v).ok())
                    .unwrap_or(0),
                output: row
                    .tokens_output
                    .and_then(|v| u64::try_from(v).ok())
                    .unwrap_or(0),
                cache_read: row
                    .tokens_cache_read
                    .and_then(|v| u64::try_from(v).ok())
                    .unwrap_or(0),
                cache_write: row
                    .tokens_cache_write
                    .and_then(|v| u64::try_from(v).ok())
                    .unwrap_or(0),
            };
            let cost_usd = row.cost.filter(|c| c.is_finite() && *c >= 0.0);
            expected.push((ms, row.directory.clone(), model, tokens, cost_usd));
        } else {
            expected_skipped += 1;
        }
        let _ = conn
            .execute(
                "INSERT INTO session_v2 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.directory,
                    row.model,
                    time_sql,
                    row.tokens_input,
                    row.tokens_output,
                    row.tokens_cache_read,
                    row.tokens_cache_write,
                    row.cost,
                ],
            )
            .expect("insert row");
    }

    let read = opencode::read_from(&conn).expect("read_from succeeds");
    assert_eq!(read.sessions.len(), expected.len());
    assert_eq!(read.skipped, expected_skipped);
    for (session, (ms, dir, model, tokens, cost)) in read.sessions.iter().zip(expected) {
        assert_eq!(session.source, Source::OpenCode);
        assert_eq!(session.started.as_millisecond(), ms);
        assert_eq!(session.project, dir);
        assert_eq!(session.model, model);
        assert_eq!(session.tokens, tokens);
        assert_eq!(session.cost_usd, cost);
    }
}

#[ephyra::test(gen = agy_transcripts(), config = droid_usage_config())]
fn agy_transcript_is_order_independent_for_earliest_time_and_sum(transcript: AgyTranscript) {
    let mut expected_started = at_ts(transcript.lines[0].ts_secs);
    let mut expected_tokens = Tokens::default();
    for line in &transcript.lines {
        expected_started = expected_started.min(at_ts(line.ts_secs));
        expected_tokens.add(&Tokens {
            input: line.input,
            output: line.output,
            cache_read: line.cache_read,
            cache_write: 0,
        });
    }

    let forward: String = transcript
        .lines
        .iter()
        .map(|l| {
            format!(
                r#"{{"created_at":"{}","input_tokens":{},"output_tokens":{},"cache_read_tokens":{}}}"#,
                at_ts(l.ts_secs).strftime("%Y-%m-%dT%H:%M:%S%.f%:z"),
                l.input,
                l.output,
                l.cache_read
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let reversed: String = transcript
        .lines
        .iter()
        .rev()
        .map(|l| {
            format!(
                r#"{{"created_at":"{}","input_tokens":{},"output_tokens":{},"cache_read_tokens":{}}}"#,
                at_ts(l.ts_secs).strftime("%Y-%m-%dT%H:%M:%S%.f%:z"),
                l.input,
                l.output,
                l.cache_read
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let a = agy::parse_transcript(Cursor::new(forward)).expect("forward parses");
    let b = agy::parse_transcript(Cursor::new(reversed)).expect("reversed parses");
    assert_eq!(a, (expected_started, expected_tokens));
    assert_eq!(a, b);
}

#[ephyra::test(gen = agy_blobs(), config = droid_usage_config())]
fn agy_first_model_name_matches_delimiter_and_prefix_oracle(blob: AgyBlob) {
    let expected = blob
        .bytes
        .split(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
        .filter_map(|token| std::str::from_utf8(token).ok())
        .find(|token| {
            ["gemini-", "claude-", "gpt-"]
                .iter()
                .any(|prefix| token.starts_with(prefix))
        })
        .map(str::to_owned);
    assert_eq!(first_model_name(&blob.bytes), expected);
}

#[expect(
    clippy::too_many_lines,
    reason = "single end-to-end invariant check across pricing, summary, report, and retain_since"
)]
#[ephyra::test(gen = session_batches(), config = droid_usage_config())]
fn pricing_summary_and_report_preserve_invariants(batch: SessionBatch) {
    let price_page = r#"{
        "gpt-5.6-sol": {"mode": "chat", "input_cost_per_token": 4e-6, "output_cost_per_token": 2e-5, "cache_read_input_token_cost": 4e-7},
        "zai/glm-5.2": {"mode": "chat", "input_cost_per_token": 1e-6, "output_cost_per_token": 4e-6},
        "gemini/gemini-3.8-flash": {"mode": "chat", "input_cost_per_token": 5e-7, "output_cost_per_token": 2e-6}
    }"#;
    let mult_page = "\
| Model | Model ID | Multiplier | Reasoning |
| --- | --- | --- | --- |
| GPT-5.6 Sol<sup>†</sup> | `gpt-5.6-sol` | 1.6× | `none` |
| GPT-5.6 Sol Fast<sup>†</sup> | `gpt-5.6-sol-fast` | 3.2× | `none` |
| GLM-5.2 | `glm-5.2` | 0.56× | `off` |
| Gemini 3.8 Flash Ultrafast | `gemini-3.8-flash-ultrafast` | 2.0× | `none` |

<Note>
<sup>†</sup> Promotional pricing. GPT-5.6 Sol bills at 1.6× through November 22, 2026, then returns to 2×.

<sup>†</sup> Promotional pricing. GPT-5.6 Sol Fast bills at 3.2× through November 22, 2026, then returns to 4×.
</Note>";
    let snap_time = at_ts(1_750_000_000);
    let prices = PriceBook::new(vec![prices::parse_table(price_page, snap_time).unwrap()]);
    let multipliers =
        MultiplierBook::new(vec![multipliers::parse_page(mult_page, snap_time).unwrap()]);
    let promo_cutoff = Date::new(2026, 11, 22).unwrap();

    let mut sessions = batch.sessions;
    let original_recorded: Vec<Option<f64>> = sessions.iter().map(|s| s.cost_usd).collect();
    price_sessions(&mut sessions, &prices, &multipliers);

    for (session, orig) in sessions.iter().zip(&original_recorded) {
        let model = session.model.as_deref();
        if session.source == Source::Droid {
            let list_key = match model {
                Some("gpt-5.6-sol" | "gpt-5.6-sol-fast") => Some("gpt-5.6-sol"),
                Some("glm-5.2") => Some("zai/glm-5.2"),
                Some("gemini-3.8-flash-ultrafast") => Some("gemini/gemini-3.8-flash"),
                _ => None,
            };
            let mult = match model {
                Some("gpt-5.6-sol") => Some(
                    if session.started.to_zoned(jiff::tz::TimeZone::UTC).date() > promo_cutoff {
                        2.0
                    } else {
                        1.6
                    },
                ),
                Some("gpt-5.6-sol-fast") => Some(
                    if session.started.to_zoned(jiff::tz::TimeZone::UTC).date() > promo_cutoff {
                        4.0
                    } else {
                        3.2
                    },
                ),
                Some("glm-5.2") => Some(0.56),
                Some("gemini-3.8-flash-ultrafast") => Some(2.0),
                _ => None,
            };
            match (list_key, mult) {
                (Some(k), Some(m)) => {
                    let rate = prices.exact_rate_at(k, session.started).unwrap();
                    let want = cost_usd(&session.tokens, &rate) * m;
                    let got = session.cost_usd.expect("known Droid model priced");
                    assert!((got - want).abs() <= want.abs() * 1e-12 + 1e-12);
                }
                _ => assert_eq!(session.cost_usd, *orig),
            }
        } else if orig.is_some() {
            assert_eq!(session.cost_usd, *orig);
        } else if let Some(rate) = model.and_then(|m| prices.rate_at(m, session.started)) {
            let want = cost_usd(&session.tokens, &rate);
            let got = session.cost_usd.expect("known non-Droid model priced");
            assert!((got - want).abs() <= want.abs() * 1e-12 + 1e-12);
        } else {
            assert_eq!(session.cost_usd, None);
        }
    }

    // Weekly conservation check (sessions and saturating per-kind tokens).
    let weeks = weekly(&sessions);
    let total_sessions: u64 = weeks.iter().map(|(_, t)| t.sessions).sum();
    assert_eq!(total_sessions, u64::try_from(sessions.len()).unwrap());
    let mut sum_tokens = Tokens::default();
    for s in &sessions {
        sum_tokens.add(&s.tokens);
    }
    let mut week_tokens = Tokens::default();
    for (_, t) in &weeks {
        week_tokens.add(&t.tokens);
    }
    assert_eq!(week_tokens, sum_tokens);

    // Plain report output check.
    let mut out = Vec::new();
    write_report(&mut out, &sessions, Colors::Off).expect("write_report succeeds");
    let text = String::from_utf8(out).expect("valid utf8 report");
    assert!(!text.contains('\x1b'));

    // Colored report also succeeds and emits bold ANSI styling.
    let mut colored_out = Vec::new();
    write_report(&mut colored_out, &sessions, Colors::On).expect("colored write_report succeeds");
    let colored_text = String::from_utf8(colored_out).expect("valid utf8 colored report");
    assert!(colored_text.contains("\u{1b}[1m"));

    // Top projects table stays aligned across rows and caps the project column at 40 display cols (+2 padding).
    let top_projects_lines: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.contains("Top projects"))
        .skip(1)
        .take_while(|l| !l.is_empty())
        .collect();
    if let Some(first_line) = top_projects_lines.first() {
        let first_width = first_line.width();
        for line in &top_projects_lines {
            assert_eq!(
                line.width(),
                first_width,
                "Top projects row widths drifted: {top_projects_lines:#?}"
            );
        }
        let project_col_width = first_line
            .split('┬')
            .next()
            .expect("border has first column")
            .trim_start_matches('┌')
            .width();
        assert!(
            project_col_width <= 42,
            "Top projects project column exceeded 42 columns ({project_col_width}): {first_line:?}"
        );
    }

    // `retain_since` exact completeness check.
    if let Some(first) = sessions.first() {
        let cutoff = first.started.to_zoned(jiff::tz::TimeZone::UTC).date();
        let expected: Vec<Session> = sessions
            .iter()
            .filter(|s| s.started.to_zoned(jiff::tz::TimeZone::UTC).date() >= cutoff)
            .cloned()
            .collect();
        let mut filtered = sessions;
        retain_since(&mut filtered, cutoff);
        assert_eq!(filtered, expected);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ExpectedCliErr {
    Unknown(String),
    InvalidSince(String),
    MissingSince,
}

#[ephyra::test(gen = cli_arg_cases(), config = droid_usage_config())]
fn cli_parse_args_matches_reference_model(case: CliArgCase) {
    let argv = case.argv();
    let got = cli::parse_args(argv.iter().map(String::as_str));

    let mut expected_mode = Mode::Online;
    let mut expected_since = None;
    let mut expected_err: Option<ExpectedCliErr> = None;
    let mut iter = argv.iter().map(String::as_str);
    while let Some(arg) = iter.next() {
        match arg {
            "--offline" => expected_mode = Mode::Offline,
            "--since" => {
                let Some(val) = iter.next() else {
                    expected_err = Some(ExpectedCliErr::MissingSince);
                    break;
                };
                if let Ok(date) = Date::strptime("%Y-%m-%d", val) {
                    expected_since = Some(date);
                } else {
                    expected_err = Some(ExpectedCliErr::InvalidSince(val.to_owned()));
                    break;
                }
            }
            other => {
                expected_err = Some(ExpectedCliErr::Unknown(other.to_owned()));
                break;
            }
        }
    }

    match (got, expected_err) {
        (Ok(parsed), None) => {
            assert_eq!(parsed.mode, expected_mode);
            assert_eq!(parsed.since, expected_since);
        }
        (Err(ArgError::Unknown(err)), Some(ExpectedCliErr::Unknown(flag))) => {
            assert!(err.to_string().contains(&flag), "{err}");
        }
        (Err(ArgError::InvalidSince(err)), Some(ExpectedCliErr::InvalidSince(bad))) => {
            assert!(err.to_string().contains(&bad), "{err}");
        }
        (Err(ArgError::MissingSince(err)), Some(ExpectedCliErr::MissingSince)) => {
            assert!(err.to_string().contains("--since"), "{err}");
        }
        (res, exp) => panic!("mismatch: got={res:?}, expected_err={exp:?}, argv={argv:?}"),
    }
}

#[ephyra::test(gen = droid_session_cases(), config = droid_usage_config())]
fn droid_settings_and_header_match_oracle(case: DroidSessionCase) {
    let settings = case.render_settings();
    let (tokens, model) = droid::parse_settings(&settings).expect("valid settings json");
    assert_eq!(
        tokens,
        Tokens {
            input: case.input.unwrap_or(0),
            output: case.output.unwrap_or(0),
            cache_read: case.cache_read.unwrap_or(0),
            cache_write: case.cache_write.unwrap_or(0),
        }
    );
    assert_eq!(model, case.model);

    let header = case.render_header();
    let mut exp_project = None;
    let mut exp_started = None;
    for line in &case.header_lines {
        match line {
            DroidHeaderLine::SessionStart { cwd } if exp_project.is_none() => {
                exp_project.clone_from(cwd);
            }
            DroidHeaderLine::Message { ts_secs } => {
                exp_started = Some(at_ts(*ts_secs));
                break;
            }
            _ => {}
        }
    }
    let got_header = droid::parse_header(Cursor::new(header));
    assert_eq!(got_header, exp_started.map(|s| (exp_project, s)));
}

#[ephyra::test(gen = multi_snapshot_cases(), config = droid_usage_config())]
fn multi_snapshot_books_pick_latest_snapshot_before_or_earliest(case: MultiSnapshotCase) {
    let mut sorted = case.fetched_secs.clone();
    sorted.sort_unstable();
    let expected_ts = sorted
        .iter()
        .copied()
        .rev()
        .find(|t| *t <= case.query_secs)
        .unwrap_or(sorted[0]);
    let expected_val = f64::from(u32::try_from(expected_ts).expect("fits u32"));

    // Encode the snapshot timestamp in the rate/multiplier so lookup reveals which snapshot won.
    let price_snaps: Vec<PriceSnapshot> = case
        .fetched_secs
        .iter()
        .map(|&secs| {
            let val = f64::from(u32::try_from(secs).expect("fits u32"));
            let mut rates = BTreeMap::new();
            let _ = rates.insert(
                "m".to_owned(),
                Rate {
                    input: val,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
            );
            PriceSnapshot {
                fetched: at_ts(secs),
                rates,
            }
        })
        .collect();
    let mult_snaps: Vec<MultSnapshot> = case
        .fetched_secs
        .iter()
        .map(|&secs| {
            let val = f64::from(u32::try_from(secs).expect("fits u32"));
            let mut models = BTreeMap::new();
            let _ = models.insert(
                "m".to_owned(),
                multipliers::Multiplier {
                    rate: val,
                    promo: None,
                },
            );
            MultSnapshot {
                fetched: at_ts(secs),
                models,
            }
        })
        .collect();

    let pbook = PriceBook::new(price_snaps);
    let mbook = MultiplierBook::new(mult_snaps);
    let query = at_ts(case.query_secs);
    assert_eq!(
        pbook.rate_at("m", query).map(|r| r.input),
        Some(expected_val)
    );
    assert_eq!(
        pbook.exact_rate_at("m", query).map(|r| r.input),
        Some(expected_val)
    );
    assert_eq!(mbook.multiplier_at("m", query), Some(expected_val));
    assert_eq!(pbook.rate_at("absent", query), None);
    assert_eq!(mbook.multiplier_at("absent", query), None);
}
