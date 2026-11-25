//! Structured generators, shrinkers, and metrics for Codex and Pi transcript readers.

use std::ops::ControlFlow;

use droid_usage::model::Tokens;
use ephyra::{Gen, GenExt, Source};
use jiff::Timestamp;
use serde_json::json;

/// One line in a synthetic Codex CLI rollout JSONL log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexLine {
    /// Session header metadata entry.
    SessionMeta {
        /// Epoch seconds timestamp.
        ts_secs: i64,
        /// Optional working directory path.
        cwd: Option<String>,
    },
    /// Turn context entry updating the active model.
    TurnContext {
        /// Optional model identifier.
        model: Option<String>,
    },
    /// Cumulative `event_msg` `token_count` event.
    TokenCount {
        /// Total input tokens (including cached).
        input: u64,
        /// Cached input tokens.
        cached: u64,
        /// Cache write input tokens.
        cache_write: u64,
        /// Output tokens.
        output: u64,
    },
    /// Cumulative `token_usage_record` event.
    UsageRecord {
        /// Total input tokens (including cached).
        input: u64,
        /// Cached input tokens.
        cached: u64,
        /// Cache write input tokens.
        cache_write: u64,
        /// Output tokens.
        output: u64,
    },
    /// `token_count` event with `info: null`.
    NullInfo,
    /// Invalid JSON line that the reader should skip.
    MalformedJson,
}

impl CodexLine {
    fn render(&self) -> String {
        match self {
            Self::SessionMeta { ts_secs, cwd } => {
                let ts = Timestamp::new(*ts_secs, 0)
                    .expect("valid timestamp")
                    .strftime("%Y-%m-%dT%H:%M:%S%.f%:z")
                    .to_string();
                match cwd {
                    Some(dir) => json!({
                        "type": "session_meta",
                        "payload": { "timestamp": ts, "cwd": dir }
                    })
                    .to_string(),
                    None => json!({
                        "type": "session_meta",
                        "payload": { "timestamp": ts }
                    })
                    .to_string(),
                }
            }
            Self::TurnContext { model } => match model {
                Some(m) => json!({ "type": "turn_context", "payload": { "model": m } }).to_string(),
                None => json!({ "type": "turn_context", "payload": {} }).to_string(),
            },
            Self::TokenCount {
                input,
                cached,
                cache_write,
                output,
            } => json!({
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": input,
                            "cached_input_tokens": cached,
                            "cache_write_input_tokens": cache_write,
                            "output_tokens": output,
                        }
                    }
                }
            })
            .to_string(),
            Self::UsageRecord {
                input,
                cached,
                cache_write,
                output,
            } => json!({
                "type": "token_usage_record",
                "payload": {
                    "thread_token_usage": {
                        "input_tokens": input,
                        "cached_input_tokens": cached,
                        "cache_write_input_tokens": cache_write,
                        "output_tokens": output,
                    }
                }
            })
            .to_string(),
            Self::NullInfo => {
                r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_owned()
            }
            Self::MalformedJson => "{not-json".to_owned(),
        }
    }
}

/// Sequence of Codex JSONL lines guaranteed to contain at least one `SessionMeta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexStream {
    /// Ordered rollout lines.
    pub lines: Vec<CodexLine>,
}

/// Reference expectations for [`CodexStream::expected`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCodexSession {
    /// Session start instant from the first `session_meta` entry.
    pub started: Timestamp,
    /// Project path from the first `session_meta` entry.
    pub project: Option<String>,
    /// Latest model from `turn_context` entries.
    pub model: Option<String>,
    /// Latest cumulative token counts.
    pub tokens: Tokens,
}

impl CodexStream {
    /// Renders the JSONL payload.
    #[must_use]
    pub fn render(&self) -> String {
        self.lines
            .iter()
            .map(CodexLine::render)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Reference oracle matching `droid_usage::codex::parse_transcript`.
    #[must_use]
    pub fn expected(&self) -> Option<ExpectedCodexSession> {
        let mut started = None;
        let mut project = None;
        let mut model = None;
        let mut tokens = Tokens::default();
        for line in &self.lines {
            match line {
                CodexLine::SessionMeta { ts_secs, cwd } if started.is_none() => {
                    started = Timestamp::new(*ts_secs, 0).ok();
                    project.clone_from(cwd);
                }
                CodexLine::TurnContext { model: Some(m) } => {
                    model = Some(m.clone());
                }
                CodexLine::TokenCount {
                    input,
                    cached,
                    cache_write,
                    output,
                }
                | CodexLine::UsageRecord {
                    input,
                    cached,
                    cache_write,
                    output,
                } => {
                    tokens = Tokens {
                        input: input.saturating_sub(*cached).saturating_sub(*cache_write),
                        output: *output,
                        cache_read: *cached,
                        cache_write: *cache_write,
                    };
                }
                _ => {}
            }
        }
        Some(ExpectedCodexSession {
            started: started?,
            project,
            model,
            tokens,
        })
    }
}

/// Predicate: a Codex stream has at least one `SessionMeta` with valid timestamp.
#[must_use]
#[ephyra::generator]
pub fn valid_codex_stream(stream: &CodexStream) -> bool {
    stream.lines.iter().any(|l| {
        matches!(l, CodexLine::SessionMeta { ts_secs, .. } if Timestamp::new(*ts_secs, 0).is_ok())
    })
}

fn draw_codex_line(g: &mut Source) -> CodexLine {
    let models = ["gpt-5.5", "gpt-5.6-sol", "o3"];
    match g.int(0u8..=5) {
        0 => CodexLine::SessionMeta {
            ts_secs: g.int(1_700_000_000i64..=1_850_000_000),
            cwd: g.bool().then(|| "/work/app".to_owned()),
        },
        1 => CodexLine::TurnContext {
            model: g.bool().then(|| (*g.pick(&models)).to_owned()),
        },
        2 => CodexLine::TokenCount {
            input: g.int(0u64..=50_000),
            cached: g.int(0u64..=60_000),
            cache_write: g.int(0u64..=10_000),
            output: g.int(0u64..=20_000),
        },
        3 => CodexLine::UsageRecord {
            input: g.int(0u64..=50_000),
            cached: g.int(0u64..=60_000),
            cache_write: g.int(0u64..=10_000),
            output: g.int(0u64..=20_000),
        },
        4 => CodexLine::NullInfo,
        _ => CodexLine::MalformedJson,
    }
}

fn draw_codex_stream(g: &mut Source) -> CodexStream {
    let extra = g.int(0usize..=12);
    let mut lines = Vec::with_capacity(extra + 1);
    lines.push(CodexLine::SessionMeta {
        ts_secs: g.int(1_700_000_000i64..=1_850_000_000),
        cwd: g.bool().then(|| "/work/main".to_owned()),
    });
    for _ in 0..extra {
        lines.push(draw_codex_line(g));
    }
    CodexStream { lines }
}

fn shrink_codex_stream(
    stream: &CodexStream,
    visit: &mut dyn FnMut(CodexStream) -> ControlFlow<()>,
) -> ControlFlow<()> {
    for idx in (0..stream.lines.len()).rev() {
        let mut next = stream.clone();
        let _ = next.lines.remove(idx);
        if valid_codex_stream(&next) {
            visit(next)?;
        }
    }
    ControlFlow::Continue(())
}

fn mutate_codex_stream(stream: &CodexStream, g: &mut Source) -> CodexStream {
    let mut next = stream.clone();
    if next.lines.len() < 16 && g.bool() {
        next.lines.push(draw_codex_line(g));
    } else if !next.lines.is_empty() {
        let idx = g.index(next.lines.len());
        let candidate = draw_codex_line(g);
        let prev = std::mem::replace(&mut next.lines[idx], candidate);
        if !valid_codex_stream(&next) {
            next.lines[idx] = prev;
        }
    }
    next
}

/// Metrics for `codex_streams`:
/// - `lines`: total JSONL lines.
/// - `usage_events`: count of `TokenCount` / `UsageRecord` lines.
#[must_use]
pub fn codex_metrics(stream: &CodexStream, _size: usize) -> Vec<(&'static str, f64)> {
    let usage = stream
        .lines
        .iter()
        .filter(|l| {
            matches!(
                l,
                CodexLine::TokenCount { .. } | CodexLine::UsageRecord { .. }
            )
        })
        .count();
    vec![
        (
            "lines",
            f64::from(u32::try_from(stream.lines.len()).unwrap_or(u32::MAX)),
        ),
        (
            "usage_events",
            f64::from(u32::try_from(usage).unwrap_or(u32::MAX)),
        ),
    ]
}

/// Generator of valid-by-construction Codex JSONL rollout streams.
#[ephyra::generator(valid_codex_stream, metrics = codex_metrics)]
pub fn codex_streams() -> impl Gen<Value = CodexStream> {
    draw_codex_stream
        .with_shrink(shrink_codex_stream)
        .with_mutate(mutate_codex_stream)
}

/// One entry in a synthetic Pi session JSONL log.
#[derive(Debug, Clone, PartialEq)]
pub enum PiLine {
    /// Session header line.
    Session {
        /// Epoch seconds timestamp.
        ts_secs: i64,
        /// Optional working directory.
        cwd: Option<String>,
    },
    /// Model change line.
    ModelChange {
        /// Optional new model ID.
        model_id: Option<String>,
    },
    /// Assistant message usage entry.
    Message {
        /// Optional model override.
        model: Option<String>,
        /// Uncached input tokens.
        input: u64,
        /// Output tokens.
        output: u64,
        /// Cached read tokens.
        cache_read: u64,
        /// Cached write tokens.
        cache_write: u64,
        /// Optional recorded cost.
        cost: Option<f64>,
    },
    /// `usage`, `compaction`, or `branch_summary` entry.
    Aux {
        /// JSON entry type name.
        kind: &'static str,
        /// Optional model name.
        model: Option<String>,
        /// Uncached input tokens.
        input: u64,
        /// Output tokens.
        output: u64,
        /// Optional recorded cost.
        cost: Option<f64>,
    },
    /// Malformed non-JSON line.
    Garbage,
}

impl PiLine {
    fn render(&self) -> String {
        match self {
            Self::Session { ts_secs, cwd } => {
                let ts = Timestamp::new(*ts_secs, 0)
                    .expect("valid timestamp")
                    .strftime("%Y-%m-%dT%H:%M:%S%.f%:z")
                    .to_string();
                match cwd {
                    Some(dir) => {
                        json!({ "type": "session", "timestamp": ts, "cwd": dir }).to_string()
                    }
                    None => json!({ "type": "session", "timestamp": ts }).to_string(),
                }
            }
            Self::ModelChange { model_id } => match model_id {
                Some(m) => json!({ "type": "model_change", "modelId": m }).to_string(),
                None => json!({ "type": "model_change" }).to_string(),
            },
            Self::Message {
                model,
                input,
                output,
                cache_read,
                cache_write,
                cost,
            } => {
                let mut usage = json!({
                    "input": input,
                    "output": output,
                    "cacheRead": cache_read,
                    "cacheWrite": cache_write,
                });
                if let Some(c) = cost {
                    usage["cost"] = json!({ "total": c });
                }
                let mut message = json!({ "role": "assistant", "usage": usage });
                if let Some(m) = model {
                    message["model"] = json!(m);
                }
                json!({ "type": "message", "message": message }).to_string()
            }
            Self::Aux {
                kind,
                model,
                input,
                output,
                cost,
            } => {
                let mut usage = json!({ "input": input, "output": output });
                if let Some(c) = cost {
                    usage["cost"] = json!({ "total": c });
                }
                let mut obj = json!({ "type": kind, "usage": usage });
                if let Some(m) = model {
                    obj["model"] = json!(m);
                }
                obj.to_string()
            }
            Self::Garbage => "not-json".to_owned(),
        }
    }
}

/// Sequence of Pi JSONL lines guaranteed to contain at least one `Session` line.
#[derive(Debug, Clone, PartialEq)]
pub struct PiStream {
    /// Ordered session transcript lines.
    pub lines: Vec<PiLine>,
}

impl PiStream {
    /// Renders the JSONL payload.
    #[must_use]
    pub fn render(&self) -> String {
        self.lines
            .iter()
            .map(PiLine::render)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Predicate: Pi stream includes at least one `Session` line with valid timestamp.
#[must_use]
#[ephyra::generator]
pub fn valid_pi_stream(stream: &PiStream) -> bool {
    stream.lines.iter().any(
        |l| matches!(l, PiLine::Session { ts_secs, .. } if Timestamp::new(*ts_secs, 0).is_ok()),
    )
}

fn draw_pi_cost(g: &mut Source) -> Option<f64> {
    match g.int(0u8..=4) {
        0 => None,
        1 => Some(0.0),
        2 => Some(-1.5),
        _ => Some(*g.pick(&[0.05, 0.25, 1.5])),
    }
}

fn draw_pi_line(g: &mut Source) -> PiLine {
    let models = ["claude-sonnet-4-5", "gemini-3.6-flash", "gpt-5.6-sol"];
    let aux_kinds = ["usage", "compaction", "branch_summary"];
    match g.int(0u8..=4) {
        0 => PiLine::Session {
            ts_secs: g.int(1_700_000_000i64..=1_850_000_000),
            cwd: g.bool().then(|| "/work/pi".to_owned()),
        },
        1 => PiLine::ModelChange {
            model_id: g.bool().then(|| (*g.pick(&models)).to_owned()),
        },
        2 => PiLine::Message {
            model: g.bool().then(|| (*g.pick(&models)).to_owned()),
            input: g.int(0u64..=10_000),
            output: g.int(0u64..=5_000),
            cache_read: g.int(0u64..=20_000),
            cache_write: g.int(0u64..=5_000),
            cost: draw_pi_cost(g),
        },
        3 => PiLine::Aux {
            kind: g.pick(&aux_kinds),
            model: g.bool().then(|| (*g.pick(&models)).to_owned()),
            input: g.int(0u64..=10_000),
            output: g.int(0u64..=5_000),
            cost: draw_pi_cost(g),
        },
        _ => PiLine::Garbage,
    }
}

fn draw_pi_stream(g: &mut Source) -> PiStream {
    let extra = g.int(0usize..=12);
    let mut lines = Vec::with_capacity(extra + 1);
    lines.push(PiLine::Session {
        ts_secs: g.int(1_700_000_000i64..=1_850_000_000),
        cwd: g.bool().then(|| "/work/pi-main".to_owned()),
    });
    for _ in 0..extra {
        lines.push(draw_pi_line(g));
    }
    PiStream { lines }
}

fn shrink_pi_stream(
    stream: &PiStream,
    visit: &mut dyn FnMut(PiStream) -> ControlFlow<()>,
) -> ControlFlow<()> {
    for idx in (0..stream.lines.len()).rev() {
        let mut next = stream.clone();
        let _ = next.lines.remove(idx);
        if valid_pi_stream(&next) {
            visit(next)?;
        }
    }
    ControlFlow::Continue(())
}

fn mutate_pi_stream(stream: &PiStream, g: &mut Source) -> PiStream {
    let mut next = stream.clone();
    if next.lines.len() < 16 && g.bool() {
        next.lines.push(draw_pi_line(g));
    } else if !next.lines.is_empty() {
        let idx = g.index(next.lines.len());
        let candidate = draw_pi_line(g);
        let prev = std::mem::replace(&mut next.lines[idx], candidate);
        if !valid_pi_stream(&next) {
            next.lines[idx] = prev;
        }
    }
    next
}

/// Metrics for `pi_streams`:
/// - `lines`: total JSONL lines.
/// - `aux_lines`: count of `usage`/`compaction`/`branch_summary` lines.
#[must_use]
pub fn pi_metrics(stream: &PiStream, _size: usize) -> Vec<(&'static str, f64)> {
    let aux = stream
        .lines
        .iter()
        .filter(|l| matches!(l, PiLine::Aux { .. }))
        .count();
    vec![
        (
            "lines",
            f64::from(u32::try_from(stream.lines.len()).unwrap_or(u32::MAX)),
        ),
        (
            "aux_lines",
            f64::from(u32::try_from(aux).unwrap_or(u32::MAX)),
        ),
    ]
}

/// Generator of valid-by-construction Pi JSONL session streams.
#[ephyra::generator(valid_pi_stream, metrics = pi_metrics)]
pub fn pi_streams() -> impl Gen<Value = PiStream> {
    draw_pi_stream
        .with_shrink(shrink_pi_stream)
        .with_mutate(mutate_pi_stream)
}
