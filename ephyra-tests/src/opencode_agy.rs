//! Structured generators, shrinkers, and metrics for `opencode` and `agy` readers.

use std::ops::ControlFlow;

use ephyra::{Gen, GenExt, Source};

/// A synthetic row in `OpenCode`'s `session_v2` `SQLite` table.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCodeRow {
    /// Optional project directory path.
    pub directory: Option<String>,
    /// Optional raw model column string (JSON or plain model name).
    pub model: Option<String>,
    /// Epoch milliseconds timestamp or malformed string when invalid.
    pub time_created: Option<Result<i64, String>>,
    /// Whether `time_created` is stored as `TEXT` instead of `INTEGER`.
    pub time_as_text: bool,
    /// Optional input token count.
    pub tokens_input: Option<i64>,
    /// Optional output token count.
    pub tokens_output: Option<i64>,
    /// Optional cache-read token count.
    pub tokens_cache_read: Option<i64>,
    /// Optional cache-write token count.
    pub tokens_cache_write: Option<i64>,
    /// Optional recorded cost.
    pub cost: Option<f64>,
}

/// Synthetic `session_v2` table contents.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCodeTable {
    /// Table rows inserted in order.
    pub rows: Vec<OpenCodeRow>,
}

/// Metrics for `opencode_tables`:
/// - `rows`: number of rows in the table.
/// - `text_times`: rows storing `time_created` as `TEXT` instead of `INTEGER`.
#[must_use]
pub fn opencode_metrics(table: &OpenCodeTable, _size: usize) -> Vec<(&'static str, f64)> {
    let text_times = table.rows.iter().filter(|r| r.time_as_text).count();
    vec![
        (
            "rows",
            f64::from(u32::try_from(table.rows.len()).unwrap_or(u32::MAX)),
        ),
        (
            "text_times",
            f64::from(u32::try_from(text_times).unwrap_or(u32::MAX)),
        ),
    ]
}

fn draw_opencode_row(g: &mut Source) -> OpenCodeRow {
    let models = [
        None,
        Some("plain-model".to_owned()),
        Some(r#"{"id":"gpt-5.5","providerID":"openai"}"#.to_owned()),
        Some(r#"{"providerID":"only"}"#.to_owned()),
    ];
    let time_created = match g.int(0u8..=3) {
        0 => None,
        1 => Some(Err("bad-timestamp".to_owned())),
        _ => Some(Ok(g.int(1_700_000_000_000i64..=1_850_000_000_000))),
    };
    let count = |g: &mut Source| -> Option<i64> {
        match g.int(0u8..=3) {
            0 => None,
            1 => Some(-5),
            _ => Some(g.int(0i64..=100_000)),
        }
    };
    let cost = match g.int(0u8..=4) {
        0 => None,
        1 => Some(-0.5),
        2 => Some(f64::INFINITY),
        3 => Some(0.0),
        _ => Some(1.25),
    };
    OpenCodeRow {
        directory: g.bool().then(|| "/work/opencode".to_owned()),
        model: g.pick(&models).clone(),
        time_created,
        time_as_text: g.bool(),
        tokens_input: count(g),
        tokens_output: count(g),
        tokens_cache_read: count(g),
        tokens_cache_write: count(g),
        cost,
    }
}

fn shrink_opencode_table(
    table: &OpenCodeTable,
    visit: &mut dyn FnMut(OpenCodeTable) -> ControlFlow<()>,
) -> ControlFlow<()> {
    for idx in (0..table.rows.len()).rev() {
        let mut next = table.rows.clone();
        let _ = next.remove(idx);
        visit(OpenCodeTable { rows: next })?;
    }
    ControlFlow::Continue(())
}

fn mutate_opencode_table(table: &OpenCodeTable, g: &mut Source) -> OpenCodeTable {
    let mut next = table.rows.clone();
    if next.is_empty() || (next.len() < 10 && g.bool()) {
        next.push(draw_opencode_row(g));
    } else {
        let idx = g.index(next.len());
        next[idx] = draw_opencode_row(g);
    }
    OpenCodeTable { rows: next }
}

/// Generator of arbitrary `OpenCode` `SQLite` `session_v2` row sets.
#[ephyra::generator(metrics = opencode_metrics)]
pub fn opencode_tables() -> impl Gen<Value = OpenCodeTable> {
    (|g: &mut Source| {
        let len = g.int(0usize..=8);
        OpenCodeTable {
            rows: (0..len).map(|_| draw_opencode_row(g)).collect(),
        }
    })
    .with_shrink(shrink_opencode_table)
    .with_mutate(mutate_opencode_table)
}

/// One synthetic Agy transcript line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyLine {
    /// Epoch seconds timestamp.
    pub ts_secs: i64,
    /// Uncached input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache-read tokens.
    pub cache_read: u64,
}

/// Non-empty synthetic Agy transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyTranscript {
    /// Transcript entries.
    pub lines: Vec<AgyLine>,
}

/// Metrics for `agy_transcripts`:
/// - `lines`: number of transcript events.
#[must_use]
pub fn agy_metrics(transcript: &AgyTranscript, _size: usize) -> Vec<(&'static str, f64)> {
    vec![(
        "lines",
        f64::from(u32::try_from(transcript.lines.len()).unwrap_or(u32::MAX)),
    )]
}

/// Predicate: Agy transcript needs at least one line.
#[must_use]
#[ephyra::generator]
pub fn valid_agy_transcript(transcript: &AgyTranscript) -> bool {
    !transcript.lines.is_empty()
}

fn draw_agy_line(g: &mut Source) -> AgyLine {
    AgyLine {
        ts_secs: g.int(1_700_000_000i64..=1_850_000_000),
        input: g.int(0u64..=50_000),
        output: g.int(0u64..=20_000),
        cache_read: g.int(0u64..=50_000),
    }
}

fn shrink_agy_transcript(
    transcript: &AgyTranscript,
    visit: &mut dyn FnMut(AgyTranscript) -> ControlFlow<()>,
) -> ControlFlow<()> {
    if transcript.lines.len() <= 1 {
        return ControlFlow::Continue(());
    }
    for idx in (0..transcript.lines.len()).rev() {
        let mut next = transcript.lines.clone();
        let _ = next.remove(idx);
        visit(AgyTranscript { lines: next })?;
    }
    ControlFlow::Continue(())
}

fn mutate_agy_transcript(transcript: &AgyTranscript, g: &mut Source) -> AgyTranscript {
    let mut next = transcript.lines.clone();
    if next.len() < 12 && g.bool() {
        next.push(draw_agy_line(g));
    } else {
        let idx = g.index(next.len());
        next[idx] = draw_agy_line(g);
    }
    AgyTranscript { lines: next }
}

/// Generator of non-empty Agy transcript line lists.
#[ephyra::generator(valid_agy_transcript, metrics = agy_metrics)]
pub fn agy_transcripts() -> impl Gen<Value = AgyTranscript> {
    (|g: &mut Source| {
        let len = g.int(1usize..=8);
        AgyTranscript {
            lines: (0..len).map(|_| draw_agy_line(g)).collect(),
        }
    })
    .with_shrink(shrink_agy_transcript)
    .with_mutate(mutate_agy_transcript)
}

/// Synthetic binary `gen_metadata` blob mixing delimiters, noise, near-misses, and known prefixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyBlob {
    /// Raw metadata bytes.
    pub bytes: Vec<u8>,
}

fn draw_blob_chunk(g: &mut Source) -> Vec<u8> {
    let tokens: [&[u8]; 9] = [
        b"gemini-3.7-flash-tiered",
        b"claude-sonnet-4-5",
        b"gpt-5.6-sol",
        b"used_claude_conservative",
        b"my-gemini-proxy",
        b"gpt",
        b"claude-\xffbad",
        b"\x00\x01\x17",
        b" /|,",
    ];
    if g.bool_with(0.8) {
        (*g.pick(&tokens)).to_vec()
    } else {
        let len = g.int(1usize..=6);
        (0..len).map(|_| g.int(0u8..=255)).collect()
    }
}

/// Metrics for `agy_blobs`:
/// - `bytes`: raw blob byte length.
#[must_use]
pub fn agy_blob_metrics(blob: &AgyBlob, _size: usize) -> Vec<(&'static str, f64)> {
    vec![(
        "bytes",
        f64::from(u32::try_from(blob.bytes.len()).unwrap_or(u32::MAX)),
    )]
}

/// Generator of structured `gen_metadata` byte blobs for `first_model_name`.
#[must_use]
#[ephyra::generator(metrics = agy_blob_metrics)]
pub fn agy_blobs() -> impl Gen<Value = AgyBlob> {
    (|g: &mut Source| {
        let chunks = g.int(0usize..=8);
        let mut bytes = Vec::new();
        for _ in 0..chunks {
            bytes.extend_from_slice(&draw_blob_chunk(g));
        }
        AgyBlob { bytes }
    })
    .with_shrink(|blob: &AgyBlob, visit| {
        let len = blob.bytes.len();
        if len > 1 {
            let half = len / 2;
            visit(AgyBlob {
                bytes: blob.bytes[..half].to_vec(),
            })?;
            visit(AgyBlob {
                bytes: blob.bytes[half..].to_vec(),
            })?;
        }
        if len > 0 {
            let mut popped = blob.bytes.clone();
            let _ = popped.pop();
            visit(AgyBlob { bytes: popped })?;
        }
        ControlFlow::Continue(())
    })
    .with_mutate(|blob: &AgyBlob, g: &mut Source| {
        let mut next = blob.bytes.clone();
        if next.is_empty() || (next.len() < 128 && g.bool()) {
            next.extend_from_slice(&draw_blob_chunk(g));
        } else {
            let idx = g.index(next.len());
            next[idx] = g.int(0u8..=255);
        }
        AgyBlob { bytes: next }
    })
}
