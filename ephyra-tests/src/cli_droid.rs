//! Structured generators, shrinkers, and metrics for `cli` and `droid` modules.

use std::ops::ControlFlow;

use ephyra::{Gen, GenExt, Source};
use jiff::Timestamp;
use jiff::civil::Date;
use serde_json::json;

/// One CLI token in a synthetic `droid-usage` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliToken {
    /// `--offline` flag.
    Offline,
    /// Valid `--since YYYY-MM-DD` flag pair.
    SinceValid(Date),
    /// `--since` followed by an invalid date string.
    SinceInvalid(String),
    /// `--since` with no following argument.
    DanglingSince,
    /// Unrecognized argument token.
    Unknown(String),
}

/// Generated argument list for `droid_usage::cli::parse_args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgCase {
    /// Ordered CLI token specs.
    pub tokens: Vec<CliToken>,
}

impl CliArgCase {
    /// Materializes raw CLI argument strings.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        let mut out = Vec::new();
        for tok in &self.tokens {
            match tok {
                CliToken::Offline => out.push("--offline".to_owned()),
                CliToken::SinceValid(d) => {
                    out.push("--since".to_owned());
                    out.push(d.strftime("%Y-%m-%d").to_string());
                }
                CliToken::SinceInvalid(bad) => {
                    out.push("--since".to_owned());
                    out.push(bad.clone());
                }
                CliToken::DanglingSince => out.push("--since".to_owned()),
                CliToken::Unknown(flag) => out.push(flag.clone()),
            }
        }
        out
    }
}

fn draw_cli_token(g: &mut Source) -> CliToken {
    let bad_dates = ["2026-02-29", "2026-13-01", "yesterday", "2026/10/24"];
    let unknowns = ["--help", "-h", "--bogus", "2026-10-24"];
    match g.int(0u8..=4) {
        0 => CliToken::Offline,
        1 => {
            let days = g.int(0i64..=2000);
            let date = Date::new(2024, 1, 1).expect("valid") + jiff::Span::new().days(days);
            CliToken::SinceValid(date)
        }
        2 => CliToken::SinceInvalid((*g.pick(&bad_dates)).to_owned()),
        3 => CliToken::DanglingSince,
        _ => CliToken::Unknown((*g.pick(&unknowns)).to_owned()),
    }
}

fn shrink_cli(
    case: &CliArgCase,
    visit: &mut dyn FnMut(CliArgCase) -> ControlFlow<()>,
) -> ControlFlow<()> {
    for idx in (0..case.tokens.len()).rev() {
        let mut next = case.tokens.clone();
        let _ = next.remove(idx);
        visit(CliArgCase { tokens: next })?;
    }
    ControlFlow::Continue(())
}

fn mutate_cli(case: &CliArgCase, g: &mut Source) -> CliArgCase {
    let mut next = case.tokens.clone();
    if next.is_empty() || (next.len() < 8 && g.bool()) {
        next.push(draw_cli_token(g));
    } else {
        let idx = g.index(next.len());
        next[idx] = draw_cli_token(g);
    }
    CliArgCase { tokens: next }
}

/// Metrics for `cli_arg_cases`:
/// - `tokens`: number of CLI token specs.
#[must_use]
pub fn cli_metrics(case: &CliArgCase, _size: usize) -> Vec<(&'static str, f64)> {
    vec![(
        "tokens",
        f64::from(u32::try_from(case.tokens.len()).unwrap_or(u32::MAX)),
    )]
}

/// Generator of synthetic CLI argument sequences.
#[ephyra::generator(metrics = cli_metrics)]
pub fn cli_arg_cases() -> impl Gen<Value = CliArgCase> {
    (|g: &mut Source| {
        let len = g.int(0usize..=6);
        CliArgCase {
            tokens: (0..len).map(|_| draw_cli_token(g)).collect(),
        }
    })
    .with_shrink(shrink_cli)
    .with_mutate(mutate_cli)
}

/// One line in a Droid session transcript header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DroidHeaderLine {
    /// `session_start` event with optional `cwd`.
    SessionStart {
        /// Optional working directory.
        cwd: Option<String>,
    },
    /// `message` event carrying a timestamp.
    Message {
        /// Epoch seconds timestamp.
        ts_secs: i64,
    },
    /// Unparseable non-JSON line.
    Malformed,
    /// Unrelated valid JSON line.
    OtherType,
}

impl DroidHeaderLine {
    fn render(&self) -> String {
        match self {
            Self::SessionStart { cwd: Some(c) } => {
                json!({ "type": "session_start", "cwd": c }).to_string()
            }
            Self::SessionStart { cwd: None } => json!({ "type": "session_start" }).to_string(),
            Self::Message { ts_secs } => {
                let ts = Timestamp::new(*ts_secs, 0)
                    .expect("valid timestamp")
                    .strftime("%Y-%m-%dT%H:%M:%S%.f%:z")
                    .to_string();
                json!({ "type": "message", "timestamp": ts }).to_string()
            }
            Self::Malformed => "{not json".to_owned(),
            Self::OtherType => json!({ "type": "tool_call" }).to_string(),
        }
    }
}

/// Paired synthetic Droid settings JSON + transcript header lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroidSessionCase {
    /// Optional model name in `.settings.json`.
    pub model: Option<String>,
    /// Optional `inputTokens` count.
    pub input: Option<u64>,
    /// Optional `outputTokens` count.
    pub output: Option<u64>,
    /// Optional `cacheReadTokens` count.
    pub cache_read: Option<u64>,
    /// Optional `cacheCreationTokens` count.
    pub cache_write: Option<u64>,
    /// Child-inclusive token count (must be ignored by `parse_settings`).
    pub inclusive_input: u64,
    /// Ordered header lines for `.jsonl`.
    pub header_lines: Vec<DroidHeaderLine>,
}

impl DroidSessionCase {
    /// Renders `.settings.json` text.
    #[must_use]
    pub fn render_settings(&self) -> String {
        let mut usage = serde_json::Map::new();
        if let Some(v) = self.input {
            let _ = usage.insert("inputTokens".to_owned(), json!(v));
        }
        if let Some(v) = self.output {
            let _ = usage.insert("outputTokens".to_owned(), json!(v));
        }
        if let Some(v) = self.cache_read {
            let _ = usage.insert("cacheReadTokens".to_owned(), json!(v));
        }
        if let Some(v) = self.cache_write {
            let _ = usage.insert("cacheCreationTokens".to_owned(), json!(v));
        }
        let mut root = serde_json::Map::new();
        if let Some(m) = &self.model {
            let _ = root.insert("model".to_owned(), json!(m));
        }
        let _ = root.insert("tokenUsage".to_owned(), serde_json::Value::Object(usage));
        let _ = root.insert(
            "inclusiveTokenUsage".to_owned(),
            json!({ "inputTokens": self.inclusive_input }),
        );
        serde_json::Value::Object(root).to_string()
    }

    /// Renders `.jsonl` header text.
    #[must_use]
    pub fn render_header(&self) -> String {
        self.header_lines
            .iter()
            .map(DroidHeaderLine::render)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn draw_header_line(g: &mut Source) -> DroidHeaderLine {
    match g.int(0u8..=3) {
        0 => DroidHeaderLine::SessionStart {
            cwd: g
                .bool()
                .then(|| (*g.pick(&["/work/droid-a", "/work/droid-b", "/tmp/other"])).to_owned()),
        },
        1 => DroidHeaderLine::Message {
            ts_secs: g.int(1_700_000_000i64..=1_850_000_000),
        },
        2 => DroidHeaderLine::Malformed,
        _ => DroidHeaderLine::OtherType,
    }
}

fn draw_droid_case(g: &mut Source) -> DroidSessionCase {
    let models = ["gpt-5.6-sol", "glm-5.2", "claude-sonnet-4-5"];
    let len = g.int(0usize..=8);
    let mut header_lines: Vec<DroidHeaderLine> = (0..len).map(|_| draw_header_line(g)).collect();
    if g.bool_with(0.7) {
        header_lines.push(DroidHeaderLine::Message {
            ts_secs: g.int(1_700_000_000i64..=1_850_000_000),
        });
    }
    DroidSessionCase {
        model: g.bool().then(|| (*g.pick(&models)).to_owned()),
        input: g.bool().then(|| g.int(0u64..=100_000)),
        output: g.bool().then(|| g.int(0u64..=50_000)),
        cache_read: g.bool().then(|| g.int(0u64..=100_000)),
        cache_write: g.bool().then(|| g.int(0u64..=50_000)),
        inclusive_input: g.int(0u64..=10_000_000),
        header_lines,
    }
}

fn shrink_droid_case(
    case: &DroidSessionCase,
    visit: &mut dyn FnMut(DroidSessionCase) -> ControlFlow<()>,
) -> ControlFlow<()> {
    for idx in (0..case.header_lines.len()).rev() {
        let mut next = case.clone();
        let _ = next.header_lines.remove(idx);
        visit(next)?;
    }
    ControlFlow::Continue(())
}

fn mutate_droid_case(case: &DroidSessionCase, g: &mut Source) -> DroidSessionCase {
    let mut next = case.clone();
    if next.header_lines.is_empty() || (next.header_lines.len() < 10 && g.bool()) {
        next.header_lines.push(draw_header_line(g));
    } else {
        let idx = g.index(next.header_lines.len());
        next.header_lines[idx] = draw_header_line(g);
    }
    next
}

/// Metrics for `droid_session_cases`:
/// - `header_lines`: count of lines in the transcript header.
#[must_use]
pub fn droid_metrics(case: &DroidSessionCase, _size: usize) -> Vec<(&'static str, f64)> {
    vec![(
        "header_lines",
        f64::from(u32::try_from(case.header_lines.len()).unwrap_or(u32::MAX)),
    )]
}

/// Generator of synthetic Droid `.settings.json` + `.jsonl` header pairs.
#[ephyra::generator(metrics = droid_metrics)]
pub fn droid_session_cases() -> impl Gen<Value = DroidSessionCase> {
    draw_droid_case
        .with_shrink(shrink_droid_case)
        .with_mutate(mutate_droid_case)
}

/// Query case over multiple dated snapshots for `PriceBook` and `MultiplierBook`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiSnapshotCase {
    /// Snapshot `fetched` epoch seconds (unsorted).
    pub fetched_secs: Vec<i64>,
    /// Query instant in epoch seconds.
    pub query_secs: i64,
}

/// Predicate: at least one snapshot timestamp and valid UTC instants.
#[must_use]
#[ephyra::generator]
pub fn valid_multi_snapshot_case(case: &MultiSnapshotCase) -> bool {
    !case.fetched_secs.is_empty()
        && case
            .fetched_secs
            .iter()
            .all(|s| Timestamp::new(*s, 0).is_ok())
        && Timestamp::new(case.query_secs, 0).is_ok()
}

/// Metrics for `multi_snapshot_cases`:
/// - `snapshots`: number of snapshots in the book.
#[must_use]
pub fn multi_snapshot_metrics(case: &MultiSnapshotCase, _size: usize) -> Vec<(&'static str, f64)> {
    vec![(
        "snapshots",
        f64::from(u32::try_from(case.fetched_secs.len()).unwrap_or(u32::MAX)),
    )]
}

/// Generator of multi-snapshot timestamps and query times.
#[must_use]
#[ephyra::generator(valid_multi_snapshot_case, metrics = multi_snapshot_metrics)]
pub fn multi_snapshot_cases() -> impl Gen<Value = MultiSnapshotCase> {
    (|g: &mut Source| {
        let n = g.int(1usize..=5);
        let fetched_secs: Vec<i64> = (0..n)
            .map(|_| g.int(1_750_000_000i64..=1_800_000_000))
            .collect();
        let query_secs = if g.bool_with(0.25) {
            // Exact hit on one of the snapshot timestamps.
            *g.pick(&fetched_secs)
        } else {
            g.int(1_740_000_000i64..=1_810_000_000)
        };
        MultiSnapshotCase {
            fetched_secs,
            query_secs,
        }
    })
    .with_shrink(
        |case: &MultiSnapshotCase, visit: &mut dyn FnMut(MultiSnapshotCase) -> ControlFlow<()>| {
            if case.fetched_secs.len() <= 1 {
                return ControlFlow::Continue(());
            }
            for idx in (0..case.fetched_secs.len()).rev() {
                let mut next = case.fetched_secs.clone();
                let _ = next.remove(idx);
                visit(MultiSnapshotCase {
                    fetched_secs: next,
                    query_secs: case.query_secs,
                })?;
            }
            ControlFlow::Continue(())
        },
    )
    .with_mutate(|case: &MultiSnapshotCase, g: &mut Source| {
        let mut next = case.clone();
        if g.bool() {
            next.query_secs = g.int(1_740_000_000i64..=1_810_000_000);
        } else if next.fetched_secs.len() < 5 {
            next.fetched_secs
                .push(g.int(1_750_000_000i64..=1_800_000_000));
        } else {
            let idx = g.index(next.fetched_secs.len());
            next.fetched_secs[idx] = g.int(1_750_000_000i64..=1_800_000_000);
        }
        next
    })
}
