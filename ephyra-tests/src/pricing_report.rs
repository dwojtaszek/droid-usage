//! Structured generators, shrinkers, and metrics for `pricing`, `summary`, and `report`.

use std::ops::ControlFlow;

use chrono::DateTime;
use droid_usage::model::{Session, Source as UsageSource, Tokens};
use ephyra::{Gen, GenExt, Source, gens};

/// A synthetic batch of sessions covering all five sources and tricky display names.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionBatch {
    /// Sessions included in the batch.
    pub sessions: Vec<Session>,
}

/// Predicate ensuring all generated `cost_usd` values are finite and non-negative.
#[must_use]
#[ephyra::generator]
pub fn valid_session_batch(batch: &SessionBatch) -> bool {
    batch
        .sessions
        .iter()
        .all(|s| s.cost_usd.is_none_or(|c| c.is_finite() && c >= 0.0))
}

fn draw_session(g: &mut Source) -> Session {
    let sources = [
        UsageSource::Droid,
        UsageSource::OpenCode,
        UsageSource::Agy,
        UsageSource::Codex,
        UsageSource::Pi,
    ];
    let source = *g.pick(&sources);
    let models = [
        None,
        Some("gpt-5.6-sol"),
        Some("gpt-5.6-sol-fast"),
        Some("glm-5.2"),
        Some("gemini-3.8-flash-ultrafast"),
        Some("(unknown)"),
        Some("unknown-model"),
    ];
    let projects = [
        None,
        Some("/work/app"),
        Some("(unknown)"),
        Some("/home/dom/orca/workspaces/zipper/very-long-project-name-for-shortening"),
        Some("/work/仕事のプロジェクトデータ分析レポート生成パイプラインの再構築"),
        Some("tab\tvertical\u{000b}👍🏻\u{200d}🔧"),
    ];
    let project = match g.int(0u8..=4) {
        0..=2 => (*g.pick(&projects)).map(str::to_owned),
        3 => Some(format!("/work/proj-{:02}", g.int(0u8..=15))),
        // Arbitrary Unicode strings (CJK, emoji, combining chars, control chars except ESC) to stress shorten().
        _ => Some(
            g.draw(&gens::string_len(gens::char(), 0..=80))
                .replace('\x1b', " "),
        ),
    };
    // Range spans 2025-10-09 through 2027-03-14, exercising both before and after the 2026-11-22 promo end date.
    let secs = g.int(1_760_000_000i64..=1_805_000_000);
    let started = DateTime::from_timestamp(secs, 0).expect("valid timestamp");
    let cost_usd = if source == UsageSource::Droid || g.bool_with(0.4) {
        None
    } else {
        Some(*g.pick(&[0.0, 0.25, 1.5, 12.345]))
    };
    let use_edge_tokens = g.bool_with(0.1);
    let tok_gen = gens::edges(0u64..=u64::MAX);
    Session {
        source,
        started,
        project,
        model: (*g.pick(&models)).map(str::to_owned),
        tokens: Tokens {
            input: if use_edge_tokens {
                g.draw(&tok_gen)
            } else {
                g.int(0u64..=1_000_000)
            },
            output: g.int(0u64..=200_000),
            cache_read: g.int(0u64..=500_000),
            cache_write: g.int(0u64..=100_000),
        },
        cost_usd,
    }
}

fn shrink_batch(
    batch: &SessionBatch,
    visit: &mut dyn FnMut(SessionBatch) -> ControlFlow<()>,
) -> ControlFlow<()> {
    for idx in (0..batch.sessions.len()).rev() {
        let mut next = batch.sessions.clone();
        let _ = next.remove(idx);
        visit(SessionBatch { sessions: next })?;
    }
    ControlFlow::Continue(())
}

fn mutate_batch(batch: &SessionBatch, g: &mut Source) -> SessionBatch {
    let mut next = batch.sessions.clone();
    if next.is_empty() || (next.len() < 24 && g.bool()) {
        next.push(draw_session(g));
    } else {
        let idx = g.index(next.len());
        next[idx] = draw_session(g);
    }
    SessionBatch { sessions: next }
}

/// Metrics for `session_batches`:
/// - `sessions`: count of sessions in the batch.
/// - `droid_share`: fraction of Droid sessions (0.0..=1.0).
#[must_use]
pub fn session_batch_metrics(batch: &SessionBatch, _size: usize) -> Vec<(&'static str, f64)> {
    let n = f64::from(u32::try_from(batch.sessions.len()).unwrap_or(u32::MAX));
    let droid = f64::from(
        u32::try_from(
            batch
                .sessions
                .iter()
                .filter(|s| s.source == UsageSource::Droid)
                .count(),
        )
        .unwrap_or(u32::MAX),
    );
    vec![("sessions", n), ("droid_share", droid / n.max(1.0))]
}

/// Generator of multi-source session batches for pricing and reporting properties.
#[ephyra::generator(valid_session_batch, metrics = session_batch_metrics)]
pub fn session_batches() -> impl Gen<Value = SessionBatch> {
    (|g: &mut Source| {
        let len = g.int(0usize..=20);
        SessionBatch {
            sessions: (0..len).map(|_| draw_session(g)).collect(),
        }
    })
    .with_shrink(shrink_batch)
    .with_mutate(mutate_batch)
}
