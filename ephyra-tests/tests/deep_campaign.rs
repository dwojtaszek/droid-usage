//! Deeper Ephyra campaign covering `app::run`, `refresh`, `snapshots`, `files`, and mutated parser/report boundaries.
#![allow(
    clippy::needless_pass_by_value,
    reason = "ephyra::test properties receive generated inputs by value"
)]

use std::fmt::Write as _;
use std::fs;
use std::io::Cursor;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use droid_usage::app::{self, SourceRefresh};
use droid_usage::model::Source as UsageSource;
use droid_usage::multipliers::{self, Snapshot as MultSnapshot};
use droid_usage::pi;
use droid_usage::prices::{self, Snapshot as PriceSnapshot};
use droid_usage::refresh;
use droid_usage::report::{Colors, write_report};
use droid_usage::snapshots::{Refresh, is_stale, last_checked};
use droid_usage_ephyra::droid_usage_config;
use droid_usage_ephyra::multipliers::{MultiplierPageCase, multiplier_pages};
use droid_usage_ephyra::prices::{PriceTableCase, price_tables};
use droid_usage_ephyra::pricing_report::{SessionBatch, session_batches};
use ephyra::{Gen, GenExt, Source, gens, loops};
use rusqlite::{Connection, params};
use unicode_width::UnicodeWidthStr;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn utc(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
}

struct TempHome(PathBuf);

impl TempHome {
    fn new(label: &str) -> Self {
        let id = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droid-usage-ephyra-{}-{label}-{id}",
            std::process::id()
        ));
        fs::create_dir(&dir).expect("create temp home");
        Self(dir)
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone)]
enum FuzzCase<T> {
    Valid(T),
    Corrupt(String),
}

struct FuzzGen<G: Gen> {
    valid: G,
    templates: &'static [&'static str],
    render: fn(&G::Value) -> String,
}

impl<G> Gen for FuzzGen<G>
where
    G: Gen,
    G::Value: Clone,
{
    type Value = FuzzCase<G::Value>;

    fn generate(&self, g: &mut Source) -> Self::Value {
        if g.bool_with(0.6) {
            FuzzCase::Valid(self.valid.generate(g))
        } else {
            let mut s = (*g.pick(self.templates)).to_owned();
            if g.bool() {
                s.push_str(&g.draw(&gens::string_len(gens::char(), 0..=30)));
            }
            FuzzCase::Corrupt(s)
        }
    }

    fn shrink(
        &self,
        value: &Self::Value,
        visit: &mut dyn FnMut(Self::Value) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        match value {
            FuzzCase::Valid(inner) => self
                .valid
                .shrink(inner, &mut |cand| visit(FuzzCase::Valid(cand))),
            FuzzCase::Corrupt(text) => {
                gens::string().shrink(text, &mut |cand| visit(FuzzCase::Corrupt(cand)))
            }
        }
    }

    fn mutate(&self, value: &Self::Value, g: &mut Source) -> Self::Value {
        match g.int(0u8..=7) {
            0 => return self.generate(g),
            1 => {
                if let FuzzCase::Valid(inner) = value {
                    let rendered = (self.render)(inner);
                    return FuzzCase::Corrupt(gens::string().mutate(&rendered, g));
                }
            }
            _ => {}
        }
        match value {
            FuzzCase::Valid(inner) => FuzzCase::Valid(self.valid.mutate(inner, g)),
            FuzzCase::Corrupt(text) => FuzzCase::Corrupt(gens::string().mutate(text, g)),
        }
    }
}

fn mutated_pages() -> impl Gen<Value = FuzzCase<MultiplierPageCase>> {
    const TEMPLATES: &[&str] = &[
        "| A | `a` | 1.5× | `none` |\n",
        "| A | `a` | 1.5× |\r\n<sup>†</sup> Promotional pricing. A bills at 1.5× through November 5, 2026, then returns to 2×.\r\n",
        "| A | `a` | 0× | `none` |",
        "| A | `a` | NaN× | `none` |",
        "| A | `a` | inf× | `none` |",
        "| A | a | 1.5× | `none` |",
        "Random prose mentioning bills at the end of the day",
        "| A | `a` | 1.5× | `none` |\n<sup>†</sup> Promotional pricing. A bills at 1.5× through February 30, 2026, then returns to 2×.",
        "| A | `a` | 1.5× | `none` |\n<sup>†</sup> Promotional pricing. Missing fields",
    ];
    FuzzGen {
        valid: multiplier_pages(),
        templates: TEMPLATES,
        render: MultiplierPageCase::render,
    }
}

#[ephyra::test(
    gen = mutated_pages(),
    search = |run| loops::fuzz(run, &mut loops::Fifo::new(), 10_000),
    config = droid_usage_config()
)]
fn multipliers_fuzz_never_yields_bad_rate_and_roundtrips(input: FuzzCase<MultiplierPageCase>) {
    let (is_valid, text) = match input {
        FuzzCase::Valid(case) => (true, case.render()),
        FuzzCase::Corrupt(s) => (false, s),
    };
    let fetched = utc(2026, 10, 24, 0);
    let res = multipliers::parse_page(&text, fetched);
    if is_valid {
        assert!(res.is_ok(), "valid multiplier page rejected: {res:?}");
    }
    if let Ok(snap) = res {
        assert!(!snap.models.is_empty());
        for (id, mult) in &snap.models {
            assert!(
                mult.rate.is_finite() && mult.rate > 0.0,
                "{id}: bad rate {}",
                mult.rate
            );
            if let Some(promo) = mult.promo {
                assert!(
                    promo.after.is_finite() && promo.after > 0.0,
                    "{id}: bad after {}",
                    promo.after
                );
            }
        }
        assert_eq!(MultSnapshot::from_json(&snap.to_json()), Some(snap));
    }
}

fn mutated_tables() -> impl Gen<Value = FuzzCase<PriceTableCase>> {
    const SNIPPETS: &[&str] = &[
        "[]",
        "{}",
        "{not json",
        r#"{"a":{"mode":"embedding","input_cost_per_token":1.0}}"#,
        r#"{"a":{"mode":"chat","input_cost_per_token":1e999,"output_cost_per_token":1.0}}"#,
        r#"{"a":{"mode":"chat","input_cost_per_token":-0.0,"output_cost_per_token":0.0}}"#,
        r#"{"a":{"mode":"responses","input_cost_per_token":1.0,"output_cost_per_token":2.0,"cache_read_input_token_cost":-1.0}}"#,
    ];
    FuzzGen {
        valid: price_tables(),
        templates: SNIPPETS,
        render: PriceTableCase::to_json_text,
    }
}

#[ephyra::test(
    gen = mutated_tables(),
    search = |run| loops::fuzz(run, &mut loops::Fifo::new(), 10_000),
    config = droid_usage_config()
)]
fn prices_fuzz_never_yields_bad_price_and_roundtrips(input: FuzzCase<PriceTableCase>) {
    let (is_valid, text) = match input {
        FuzzCase::Valid(case) => (true, case.to_json_text()),
        FuzzCase::Corrupt(s) => (false, s),
    };
    let fetched = utc(2026, 10, 24, 0);
    let res = prices::parse_table(&text, fetched);
    if is_valid {
        assert!(res.is_ok(), "valid price table rejected: {res:?}");
    }
    if let Ok(snap) = res {
        assert!(!snap.rates.is_empty());
        for (key, rate) in &snap.rates {
            for price in [rate.input, rate.output, rate.cache_read, rate.cache_write] {
                assert!(
                    price.is_finite() && price >= 0.0,
                    "{key}: bad price {price}"
                );
            }
        }
        assert_eq!(PriceSnapshot::from_json(&snap.to_json()), Some(snap));
    }
}

#[derive(Debug, Clone)]
struct PiExtremeCostInput {
    costs: Vec<f64>,
}

fn pi_extreme_costs() -> impl Gen<Value = PiExtremeCostInput> {
    (|g: &mut Source| {
        let n = g.int(1usize..=5);
        let choices = [0.0, -1.0, 0.25, 1e100, 1e308];
        PiExtremeCostInput {
            costs: (0..n).map(|_| *g.pick(&choices)).collect(),
        }
    })
    .with_shrink(|v: &PiExtremeCostInput, visit| {
        if v.costs.len() > 1 {
            for idx in (0..v.costs.len()).rev() {
                let mut next = v.costs.clone();
                let _ = next.remove(idx);
                visit(PiExtremeCostInput { costs: next })?;
            }
        }
        std::ops::ControlFlow::Continue(())
    })
}

#[ephyra::test(gen = pi_extreme_costs(), config = droid_usage_config().tests(2_000))]
fn pi_recorded_cost_stays_finite_when_present(input: PiExtremeCostInput) {
    let mut text =
        String::from(r#"{"type":"session","timestamp":"2026-10-05T15:04:35Z","cwd":"/work/app"}"#);
    for c in &input.costs {
        let _ = write!(
            text,
            "\n{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"model\":\"m\",\"usage\":{{\"input\":10,\"output\":5,\"cost\":{{\"total\":{c}}}}}}}}}"
        );
    }
    let session = pi::parse_transcript(Cursor::new(text)).expect("valid session header");
    let expected_cost: f64 = input.costs.iter().copied().filter(|cost| *cost > 0.0).sum();
    assert_eq!(
        session.cost_usd.is_some(),
        expected_cost.is_finite() && expected_cost > 0.0
    );
    if let Some(cost) = session.cost_usd {
        assert!(
            cost.is_finite() && cost >= 0.0,
            "non-finite or negative pi cost_usd: {cost}"
        );
    }
}

#[derive(Debug, Clone)]
struct AppAndRefreshCase {
    since_cutoff: Option<NaiveDate>,
    fail_mult_save: bool,
    corrupt_snapshot: bool,
    include_bad_entries: bool,
    rate_input: f64,
}

fn app_refresh_cases() -> impl Gen<Value = AppAndRefreshCase> {
    |g: &mut Source| {
        let since_cutoff = g
            .bool()
            .then(|| NaiveDate::from_ymd_opt(2026, 10, g.int(1u32..=28)).expect("valid day"));
        AppAndRefreshCase {
            since_cutoff,
            fail_mult_save: g.bool_with(0.3),
            corrupt_snapshot: g.bool_with(0.3),
            include_bad_entries: g.bool_with(0.4),
            rate_input: *g.pick(&[1e-6, 2e-6, 4e-6]),
        }
    }
}

#[ephyra::test(gen = app_refresh_cases(), config = droid_usage_config().tests(300))]
fn app_and_refresh_preserve_atomicity_and_report_across_sources(case: AppAndRefreshCase) {
    let home = TempHome::new("app-refresh");
    let data = home.0.join(".local/share/droid-usage");
    let prices_dir = data.join("prices");
    let mult_dir = data.join("multipliers");

    let table_v1 = format!(
        r#"{{"claude-opus-4-6":{{"mode":"chat","input_cost_per_token":{},"output_cost_per_token":8e-6}}}}"#,
        case.rate_input
    );
    let page_v1 = "| Claude Opus 4.6 | `claude-opus-4-6` | 1.5× | `none` |";
    let t0 = utc(2026, 10, 10, 12);

    // Initial valid apply succeeds and writes check timestamp.
    refresh::apply(&prices_dir, &mult_dir, t0, &table_v1, page_v1).expect("initial apply succeeds");
    assert!(!is_stale(&prices_dir, t0));
    assert_eq!(last_checked(&prices_dir), Some(t0));

    // Second apply with identical contents is Refresh::Unchanged at snapshots layer.
    let snap = prices::parse_table(&table_v1, utc(2026, 10, 11, 12)).unwrap();
    assert_eq!(
        prices::save(&prices_dir, &snap).unwrap(),
        Refresh::Unchanged
    );

    if case.fail_mult_save {
        fs::remove_dir_all(&mult_dir).unwrap();
        fs::write(&mult_dir, "blocking file").unwrap();
        let t1 = utc(2026, 10, 12, 12);
        let changed = table_v1.replace("8e-6", "9e-6");
        assert!(refresh::apply(&prices_dir, &mult_dir, t1, &changed, page_v1).is_err());
        // Check timestamp and previous effective prices remain intact.
        assert_eq!(last_checked(&prices_dir), Some(t0));
        let loaded = prices::read(&prices_dir).unwrap();
        assert!(
            (loaded.book.rate_at("claude-opus-4-6", t1).unwrap().output - 8e-6).abs() < 1e-12,
            "rolled back new price snapshot"
        );
        fs::remove_file(&mult_dir).unwrap();
        let _ =
            multipliers::save(&mult_dir, &multipliers::parse_page(page_v1, t0).unwrap()).unwrap();
    }

    if case.corrupt_snapshot {
        fs::write(prices_dir.join("broken.json"), "{not-json").unwrap();
    }

    populate_sources(&home.0, case.include_bad_entries);

    let (mut out, mut err) = (Vec::new(), Vec::new());
    app::run(
        &home.0,
        SourceRefresh::offline(),
        case.since_cutoff,
        utc(2026, 10, 20, 0),
        Colors::Off,
        &mut out,
        &mut err,
    )
    .expect("app::run succeeds");

    let out_text = String::from_utf8(out).expect("valid utf8 report");
    let err_text = String::from_utf8(err).expect("valid utf8 warnings");
    assert!(out_text.contains("By source"));
    if case.corrupt_snapshot {
        assert!(err_text.contains("broken.json"), "{err_text}");
    } else {
        assert!(!err_text.contains("broken.json"), "{err_text}");
    }
    if case.include_bad_entries {
        assert!(err_text.contains("droid: skipped 1"), "{err_text}");
        assert!(err_text.contains("codex: skipped 1"), "{err_text}");
    } else {
        assert!(
            !err_text.contains("skipped 1 unreadable entries"),
            "{err_text}"
        );
    }
    let sources = [
        UsageSource::Droid,
        UsageSource::OpenCode,
        UsageSource::Agy,
        UsageSource::Codex,
        UsageSource::Pi,
    ];
    if case
        .since_cutoff
        .is_some_and(|d| d > NaiveDate::from_ymd_opt(2026, 10, 15).unwrap())
    {
        assert!(!out_text.contains("/work/p1"), "{out_text}");
        for src in sources {
            assert!(
                !out_text.contains(src.as_str()),
                "unexpected {} after cutoff:\n{out_text}",
                src.as_str()
            );
        }
    } else {
        let expected_droid_cost = format!("{:.2}", 1_000_000.0 * case.rate_input * 1.5);
        assert!(out_text.contains("/work/p1"), "{out_text}");
        assert!(
            out_text.contains(&expected_droid_cost),
            "missing priced Droid cost {expected_droid_cost}:\n{out_text}"
        );
        for src in sources {
            assert!(
                out_text.contains(src.as_str()),
                "missing {}:\n{out_text}",
                src.as_str()
            );
        }
    }
}

fn populate_sources(home: &Path, include_bad_entries: bool) {
    let droid_dir = home.join(".factory/sessions/proj");
    fs::create_dir_all(&droid_dir).unwrap();
    fs::write(
        droid_dir.join("s1.settings.json"),
        r#"{"model":"claude-opus-4-6","tokenUsage":{"inputTokens":1000000,"outputTokens":0}}"#,
    )
    .unwrap();
    fs::write(
        droid_dir.join("s1.jsonl"),
        "{\"type\":\"session_start\",\"cwd\":\"/work/p1\"}\n{\"type\":\"message\",\"timestamp\":\"2026-10-15T12:00:00Z\"}\n",
    )
    .unwrap();

    let codex_dir = home.join(".codex/sessions/2026/10/15");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
        codex_dir.join("c1.jsonl"),
        "{\"type\":\"session_meta\",\"payload\":{\"timestamp\":\"2026-10-15T12:00:00Z\",\"cwd\":\"/work/p1\"}}\n{\"type\":\"turn_context\",\"payload\":{\"model\":\"claude-opus-4-6\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":50,\"output_tokens\":25}}}}\n",
    )
    .unwrap();

    let pi_dir = home.join(".pi/agent/sessions/--work-p1--");
    fs::create_dir_all(&pi_dir).unwrap();
    fs::write(
        pi_dir.join("p1.jsonl"),
        "{\"type\":\"session\",\"timestamp\":\"2026-10-15T12:00:00Z\",\"cwd\":\"/work/p1\"}\n{\"type\":\"message\",\"message\":{\"model\":\"claude-opus-4-6\",\"usage\":{\"input\":40,\"output\":10}}}\n",
    )
    .unwrap();

    let agy_logs = home.join(".gemini/antigravity-cli/brain/conv1/.system_generated/logs");
    fs::create_dir_all(&agy_logs).unwrap();
    fs::write(
        agy_logs.join("transcript.jsonl"),
        r#"{"created_at":"2026-10-15T12:00:00Z","input_tokens":30,"output_tokens":10}"#,
    )
    .unwrap();

    let oc_db = home.join(".local/share/opencode/opencode.db");
    fs::create_dir_all(oc_db.parent().unwrap()).unwrap();
    let conn = Connection::open(&oc_db).unwrap();
    conn.execute_batch(
        "CREATE TABLE session_v2 (directory TEXT, model TEXT, time_created TEXT, tokens_input INTEGER, tokens_output INTEGER, tokens_cache_read INTEGER, tokens_cache_write INTEGER, cost REAL);",
    )
    .unwrap();
    let _ = conn
        .execute(
            "INSERT INTO session_v2 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "/work/p1",
                "claude-opus-4-6",
                1_792_065_600_000i64,
                20,
                5,
                0,
                0,
                0.1
            ],
        )
        .unwrap();
    drop(conn);

    if include_bad_entries {
        fs::write(droid_dir.join("bad.settings.json"), "{bad").unwrap();
        fs::write(codex_dir.join("bad.jsonl"), "not-a-session").unwrap();
    }
}

#[ephyra::test(gen = session_batches(), config = droid_usage_config().tests(2_000))]
fn report_all_tables_keep_uniform_row_widths(batch: SessionBatch) {
    let mut out = Vec::new();
    write_report(&mut out, &batch.sessions, Colors::Off).expect("write_report succeeds");
    let text = String::from_utf8(out).expect("valid utf8");
    let mut current_width: Option<usize> = None;
    let mut tables = 0usize;
    for line in text.lines() {
        if line.starts_with('┌') {
            tables += 1;
        }
        if line.starts_with(['┌', '│', '╞', '└']) {
            let w = line.width();
            if let Some(prev) = current_width {
                assert_eq!(w, prev, "table row width drifted:\n{text}");
            } else {
                current_width = Some(w);
            }
        } else {
            current_width = None;
        }
    }
    assert_eq!(tables, 4, "expected 4 rendered tables:\n{text}");
}
