//! Structured generators, shrinkers, and metrics for `droid_usage::prices`.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use ephyra::{Gen, GenExt, Source, gens};
use jiff::Timestamp;
use serde_json::{Map, Value, json};

/// Entry kind inside a `LiteLLM` pricing JSON object.
#[derive(Debug, Clone, PartialEq)]
pub enum PriceEntryKind {
    /// Entry with `"mode": "chat"`.
    Chat {
        /// Prompt token price.
        input: f64,
        /// Completion token price.
        output: f64,
        /// Optional cached prompt read price.
        cache_read: Option<f64>,
        /// Optional cache creation write price.
        cache_write: Option<f64>,
    },
    /// Entry with `"mode": "responses"`.
    Responses {
        /// Prompt token price.
        input: f64,
        /// Completion token price.
        output: f64,
        /// Optional cached prompt read price.
        cache_read: Option<f64>,
        /// Optional cache creation write price.
        cache_write: Option<f64>,
    },
    /// Embedding-mode entry (ignored by `parse_table`).
    Embedding {
        /// Prompt token price.
        input: f64,
    },
    /// Chat entry missing required input/output rates (ignored by `parse_table`).
    MissingPrices,
}

/// A valid-by-construction `LiteLLM` pricing table plus query target.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceTableCase {
    /// Snapshot fetch timestamp.
    pub fetched: Timestamp,
    /// Model key -> pricing entry mapping.
    pub entries: BTreeMap<String, PriceEntryKind>,
    /// Bare model key queried against `PriceBook`.
    pub query_bare: String,
}

impl PriceTableCase {
    /// Renders the JSON document accepted by `droid_usage::prices::parse_table`.
    #[must_use]
    pub fn to_json_text(&self) -> String {
        let mut root = Map::new();
        for (key, kind) in &self.entries {
            let entry = match kind {
                PriceEntryKind::Chat {
                    input,
                    output,
                    cache_read,
                    cache_write,
                } => chat_value("chat", *input, *output, *cache_read, *cache_write),
                PriceEntryKind::Responses {
                    input,
                    output,
                    cache_read,
                    cache_write,
                } => chat_value("responses", *input, *output, *cache_read, *cache_write),
                PriceEntryKind::Embedding { input } => json!({
                    "mode": "embedding",
                    "input_cost_per_token": input,
                    "output_cost_per_token": 0.0,
                }),
                PriceEntryKind::MissingPrices => json!({ "mode": "chat" }),
            };
            let _ = root.insert(key.clone(), entry);
        }
        Value::Object(root).to_string()
    }
}

fn chat_value(
    mode: &str,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
) -> Value {
    let mut obj = Map::new();
    let _ = obj.insert("mode".to_owned(), json!(mode));
    let _ = obj.insert("input_cost_per_token".to_owned(), json!(input));
    let _ = obj.insert("output_cost_per_token".to_owned(), json!(output));
    if let Some(v) = cache_read {
        let _ = obj.insert("cache_read_input_token_cost".to_owned(), json!(v));
    }
    if let Some(v) = cache_write {
        let _ = obj.insert("cache_creation_input_token_cost".to_owned(), json!(v));
    }
    Value::Object(obj)
}

fn valid_rate(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

/// Predicate ensuring at least one valid chat/responses entry and non-negative finite rates.
#[must_use]
#[ephyra::generator]
pub fn valid_price_table_case(case: &PriceTableCase) -> bool {
    let mut usable = 0usize;
    for kind in case.entries.values() {
        match kind {
            PriceEntryKind::Chat {
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
            } => {
                if !valid_rate(*input)
                    || !valid_rate(*output)
                    || cache_read.is_some_and(|v| !valid_rate(v))
                    || cache_write.is_some_and(|v| !valid_rate(v))
                {
                    return false;
                }
                usable += 1;
            }
            PriceEntryKind::Embedding { input } => {
                if !valid_rate(*input) {
                    return false;
                }
            }
            PriceEntryKind::MissingPrices => {}
        }
    }
    usable > 0
}

fn price_gen() -> impl Gen<Value = f64> {
    gens::pick_from(vec![0.0, 1e-8, 2e-7, 1e-6, 4e-6, 1.5e-5, 1.0])
}

fn key_gen() -> impl Gen<Value = String> {
    gens::pick_from(vec![
        "gpt-x".to_owned(),
        "openai/gpt-x".to_owned(),
        "azure/gpt-x".to_owned(),
        "azure/us/gpt-x".to_owned(),
        "vertex/gpt-x".to_owned(),
        "claude-y".to_owned(),
        "anthropic/claude-y".to_owned(),
        "embed-1".to_owned(),
        "no-price".to_owned(),
    ])
}

fn draw_price_table(g: &mut Source) -> PriceTableCase {
    let n = g.int(1usize..=8);
    let mut entries = BTreeMap::new();
    // Guarantee at least one usable chat/responses entry so valid_price_table_case holds.
    let first_key = g.draw(&key_gen());
    let _ = entries.insert(
        first_key,
        PriceEntryKind::Chat {
            input: g.draw(&price_gen()),
            output: g.draw(&price_gen()),
            cache_read: g.bool().then(|| g.draw(&price_gen())),
            cache_write: g.bool().then(|| g.draw(&price_gen())),
        },
    );
    for _ in 1..n {
        let key = g.draw(&key_gen());
        let entry = match g.int(0u8..=3) {
            0 => PriceEntryKind::Chat {
                input: g.draw(&price_gen()),
                output: g.draw(&price_gen()),
                cache_read: g.bool().then(|| g.draw(&price_gen())),
                cache_write: g.bool().then(|| g.draw(&price_gen())),
            },
            1 => PriceEntryKind::Responses {
                input: g.draw(&price_gen()),
                output: g.draw(&price_gen()),
                cache_read: g.bool().then(|| g.draw(&price_gen())),
                cache_write: g.bool().then(|| g.draw(&price_gen())),
            },
            2 => PriceEntryKind::Embedding {
                input: g.draw(&price_gen()),
            },
            _ => PriceEntryKind::MissingPrices,
        };
        let _ = entries.entry(key).or_insert(entry);
    }
    PriceTableCase {
        fetched: Timestamp::new(1_760_000_000, 0).expect("valid timestamp"),
        entries,
        query_bare: if g.bool() {
            "gpt-x".to_owned()
        } else {
            "claude-y".to_owned()
        },
    }
}

fn shrink_price_table(
    case: &PriceTableCase,
    visit: &mut dyn FnMut(PriceTableCase) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let keys: Vec<String> = case.entries.keys().cloned().collect();
    if keys.len() > 1 {
        for key in keys {
            let mut next = case.clone();
            let _ = next.entries.remove(&key);
            if valid_price_table_case(&next) {
                visit(next)?;
            }
        }
    }
    ControlFlow::Continue(())
}

fn mutate_price_table(case: &PriceTableCase, g: &mut Source) -> PriceTableCase {
    let mut next = case.clone();
    let keys: Vec<String> = next.entries.keys().cloned().collect();
    let mut changed = false;
    if !keys.is_empty() && g.bool() {
        let key = &keys[g.index(keys.len())];
        if let Some(
            PriceEntryKind::Chat { input, .. }
            | PriceEntryKind::Responses { input, .. }
            | PriceEntryKind::Embedding { input },
        ) = next.entries.get_mut(key)
        {
            *input = g.draw(&price_gen());
            changed = true;
        }
    }
    if !changed {
        next.query_bare = if next.query_bare == "gpt-x" {
            "claude-y".to_owned()
        } else {
            "gpt-x".to_owned()
        };
    }
    next
}

/// Metrics for `price_tables`:
/// - `entries`: total entries in the JSON object.
/// - `usable`: entries with `chat`/`responses` input+output prices.
#[must_use]
pub fn price_table_metrics(case: &PriceTableCase, _size: usize) -> Vec<(&'static str, f64)> {
    let usable = case
        .entries
        .values()
        .filter(|e| {
            matches!(
                e,
                PriceEntryKind::Chat { .. } | PriceEntryKind::Responses { .. }
            )
        })
        .count();
    vec![
        (
            "entries",
            f64::from(u32::try_from(case.entries.len()).unwrap_or(u32::MAX)),
        ),
        (
            "usable",
            f64::from(u32::try_from(usable).unwrap_or(u32::MAX)),
        ),
    ]
}

/// Generator of valid-by-construction `LiteLLM` pricing tables.
#[ephyra::generator(valid_price_table_case, metrics = price_table_metrics)]
pub fn price_tables() -> impl Gen<Value = PriceTableCase> {
    draw_price_table
        .with_shrink(shrink_price_table)
        .with_mutate(mutate_price_table)
}
