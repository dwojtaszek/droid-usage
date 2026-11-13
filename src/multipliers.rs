//! Droid model multipliers from Factory's models page, kept as dated local snapshots beside the price table.
//!
//! Droid bills a session at the model's list price times its multiplier. Factory lists the multipliers on its models page, with promotions that end on a fixed date. A snapshot is the page's table at one moment. It is written only when it changes, and a session uses the snapshot in effect when it started.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::Path;

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde_json::{Map, Value, json};

use crate::model::{Session, Source, parse_timestamp};
use crate::prices::{
    Loaded, PriceBook, Refresh, cost_usd, read_snapshots, snapshot_in_effect, write_snapshot,
};

/// The models page this module reads.
pub const SOURCE_URL: &str = "https://docs.factory.com/models.md";

/// The `LiteLLM` key holding each Droid model's first-party list price. A model not listed here is unpriced, because a third-party host's price is not Droid's basis.
const LIST_KEYS: &[(&str, &str)] = &[
    ("claude-fable-5", "claude-fable-5"),
    ("claude-opus-5-5", "claude-opus-5-5"),
    ("claude-opus-5", "claude-opus-5"),
    ("claude-opus-4-8", "claude-opus-4-8"),
    ("claude-opus-4-7", "claude-opus-4-7"),
    ("claude-opus-4-6", "claude-opus-4-6"),
    ("claude-opus-4-5-20251101", "claude-opus-4-5-20251101"),
    ("claude-sonnet-5-5", "claude-sonnet-5-5"),
    ("claude-sonnet-5", "claude-sonnet-5"),
    ("claude-sonnet-4-6", "claude-sonnet-4-6"),
    ("claude-sonnet-4-5-20250929", "claude-sonnet-4-5-20250929"),
    ("claude-haiku-5-5", "claude-haiku-5-5"),
    ("claude-haiku-4-5-20251001", "claude-haiku-4-5-20251001"),
    ("gpt-6.1-sol", "gpt-6.1-sol"),
    ("gpt-6-astra", "gpt-6-astra"),
    ("gpt-6-sol", "gpt-6-sol"),
    ("gpt-6-luna", "gpt-6-luna"),
    ("gpt-5.6-sol", "gpt-5.6-sol"),
    ("gpt-5.6-terra", "gpt-5.6-terra"),
    ("gpt-5.6-luna", "gpt-5.6-luna"),
    ("gpt-5.5", "gpt-5.5"),
    ("gpt-5.5-pro", "gpt-5.5-pro"),
    ("gpt-5.4", "gpt-5.4"),
    ("gpt-5.4-mini", "gpt-5.4-mini"),
    ("gpt-5.3-codex", "gpt-5.3-codex"),
    ("gpt-5.2", "gpt-5.2"),
    ("gemini-3.1-pro-preview", "gemini/gemini-3.1-pro-preview"),
    ("gemini-3.8-flash", "gemini/gemini-3.8-flash"),
    ("gemini-3.7-flash", "gemini/gemini-3.7-flash"),
    ("gemini-3.6-flash", "gemini/gemini-3.6-flash"),
    ("gemini-3.5-flash", "gemini/gemini-3.5-flash"),
    ("gemini-3-flash-preview", "gemini/gemini-3-flash-preview"),
    ("grok-4.7", "xai/grok-4.7"),
    ("grok-4.6", "xai/grok-4.6"),
    ("grok-4.5", "xai/grok-4.5"),
    ("mistral-large-4", "mistral/mistral-large-4"),
    ("mistral-medium-3.5", "mistral/mistral-medium-3.5"),
    ("glm-5.3-flash", "zai/glm-5.3-flash"),
    ("glm-5.3", "zai/glm-5.3"),
    ("glm-5.2", "zai/glm-5.2"),
    ("glm-5.1", "zai/glm-5.1"),
    ("kimi-k3", "moonshot/kimi-k3"),
    ("kimi-k2.5", "moonshot/kimi-k2.5"),
    ("deepseek-v4-pro", "deepseek/deepseek-v4-pro"),
];

/// A promotion that ends on a day. Sessions that start on or before `through` (UTC) pay the main multiplier. Later ones pay `after`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Promo {
    /// Last day of the promotion, in UTC.
    pub through: NaiveDate,
    /// Multiplier once the promotion has ended.
    pub after: f64,
}

/// One model's multiplier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Multiplier {
    /// Multiplier before any promotion ends.
    pub rate: f64,
    /// Promotion that changes the multiplier, if any.
    pub promo: Option<Promo>,
}

impl Multiplier {
    /// The multiplier for a session that started on `day` (UTC).
    #[must_use]
    pub fn on(&self, day: NaiveDate) -> f64 {
        match self.promo {
            Some(promo) if day > promo.through => promo.after,
            _ => self.rate,
        }
    }
}

/// Every model's multiplier as read at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// When the page was fetched (UTC).
    pub fetched: DateTime<Utc>,
    /// Multipliers keyed by Droid model ID.
    pub models: BTreeMap<String, Multiplier>,
}

/// Why the models page could not be used.
#[derive(Debug)]
pub enum PageError {
    /// The page has no model rows.
    NoModels,
    /// A model ID is listed in two rows. Holds the ID.
    DuplicateModel(String),
    /// A promotion note is malformed, or it does not match exactly one model row. Holds the note's line.
    Note(String),
}

impl fmt::Display for PageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoModels => write!(f, "models page has no model rows"),
            Self::DuplicateModel(id) => write!(f, "models page lists model {id} more than once"),
            Self::Note(line) => write!(f, "models page has an unreadable promotion note: {line}"),
        }
    }
}

impl Error for PageError {}

/// A promotion note read from the page, before it is matched to a model row.
struct Note {
    line: String,
    name: String,
    rate: f64,
    promo: Promo,
}

/// Reads the models page into a snapshot. Model rows look like `| Name | `id` | 0.8× | ... |`. Promotions come from notes such as `bills at 1.6× through November 22, 2026, then returns to 2×.`
///
/// # Errors
///
/// Returns an error when the page has no model rows, when a model ID is listed twice, or when a promotion note is malformed or does not match exactly one row.
pub fn parse_page(text: &str, fetched: DateTime<Utc>) -> Result<Snapshot, PageError> {
    let mut models = BTreeMap::new();
    let mut ids_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut notes = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some((name, id, rate)) = model_row(line) {
            ids_by_name
                .entry(name.to_owned())
                .or_default()
                .push(id.to_owned());
            if models
                .insert(id.to_owned(), Multiplier { rate, promo: None })
                .is_some()
            {
                return Err(PageError::DuplicateModel(id.to_owned()));
            }
        } else if line.contains(" bills at ") {
            notes.push(note(line)?);
        }
    }
    if models.is_empty() {
        return Err(PageError::NoModels);
    }
    for note in notes {
        let unmatched = || PageError::Note(note.line.clone());
        // A note names a row only when exactly one row carries that name.
        let [id] = ids_by_name.get(&note.name).map_or(&[][..], Vec::as_slice) else {
            return Err(unmatched());
        };
        let entry = models.get_mut(id).ok_or_else(unmatched)?;
        // Both rates come from page text, so the same value parses to the same number.
        if entry.rate.total_cmp(&note.rate).is_ne() {
            return Err(unmatched());
        }
        entry.promo = Some(note.promo);
    }
    Ok(Snapshot { fetched, models })
}

/// The display name, model ID, and multiplier of a model row, or `None` for any other line.
fn model_row(line: &str) -> Option<(&str, &str, f64)> {
    if !line.starts_with('|') {
        return None;
    }
    let mut cells = line.split('|').map(str::trim).skip(1);
    let name = cells.next()?;
    let id = cells.next()?.strip_prefix('`')?.strip_suffix('`')?;
    let rate = multiplier_text(cells.next()?)?;
    Some((plain_name(name), id, rate))
}

/// A display name without its footnote marker, such as `<sup>†</sup>`.
fn plain_name(name: &str) -> &str {
    name.split('<').next().unwrap_or(name).trim()
}

/// A multiplier written as a number followed by `×`. Only finite positive numbers pass.
fn multiplier_text(text: &str) -> Option<f64> {
    valid_multiplier(text.strip_suffix('×')?.parse().ok()?)
}

/// Passes only finite positive multipliers, for both page text and saved snapshots.
fn valid_multiplier(rate: f64) -> Option<f64> {
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

/// Reads a promotion note such as `<sup>†</sup> Promotional pricing. GPT-5.6 Sol bills at 1.6× through November 22, 2026, then returns to 2×.`
#[expect(
    clippy::map_err_ignore,
    reason = "any date parse failure reports the full note line via PageError::Note"
)]
fn note(line: &str) -> Result<Note, PageError> {
    let fail = || PageError::Note(line.to_owned());
    let text = line
        .split_once("Promotional pricing. ")
        .map(|(_, text)| text)
        .ok_or_else(fail)?;
    let (name, rest) = text.split_once(" bills at ").ok_or_else(fail)?;
    let (rate, rest) = rest.split_once(" through ").ok_or_else(fail)?;
    let (through, after) = rest.split_once(", then returns to ").ok_or_else(fail)?;
    Ok(Note {
        line: line.to_owned(),
        name: plain_name(name).to_owned(),
        rate: multiplier_text(rate).ok_or_else(fail)?,
        promo: Promo {
            through: NaiveDate::parse_from_str(through, "%B %d, %Y").map_err(|_| fail())?,
            after: multiplier_text(after.trim_end_matches('.')).ok_or_else(fail)?,
        },
    })
}

impl Snapshot {
    /// The snapshot as stored on disk.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let models: Map<String, Value> = self
            .models
            .iter()
            .map(|(id, multiplier)| {
                let promo = multiplier.promo.map_or(Value::Null, |promo| {
                    json!({
                        "through": promo.through.format("%Y-%m-%d").to_string(),
                        "after": promo.after,
                    })
                });
                (
                    id.clone(),
                    json!({ "rate": multiplier.rate, "promo": promo }),
                )
            })
            .collect();
        json!({
            "fetched": self.fetched.to_rfc3339_opts(SecondsFormat::Secs, true),
            "models": models,
        })
    }

    /// Reads a snapshot back from the form `to_json` writes. Returns `None` when a field is missing or a multiplier is invalid.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        let fetched = parse_timestamp(value["fetched"].as_str()?)?;
        let models = value["models"]
            .as_object()?
            .iter()
            .map(|(id, entry)| {
                let promo = match &entry["promo"] {
                    Value::Null => None,
                    promo => Some(Promo {
                        through: NaiveDate::parse_from_str(promo["through"].as_str()?, "%Y-%m-%d")
                            .ok()?,
                        after: valid_multiplier(promo["after"].as_f64()?)?,
                    }),
                };
                let multiplier = Multiplier {
                    rate: valid_multiplier(entry["rate"].as_f64()?)?,
                    promo,
                };
                Some((id.clone(), multiplier))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        (!models.is_empty()).then_some(Self { fetched, models })
    }
}

/// All saved snapshots, oldest first.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MultiplierBook {
    snapshots: Vec<Snapshot>,
}

impl MultiplierBook {
    /// A book holding `snapshots`, sorted oldest first.
    #[must_use]
    pub fn new(mut snapshots: Vec<Snapshot>) -> Self {
        snapshots.sort_by_key(|snapshot| snapshot.fetched);
        Self { snapshots }
    }

    /// True when no snapshot is saved.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// The multiplier for `model` for a session that started at `at`, from the snapshot in effect at that time. Uses the earliest snapshot when `at` comes before all of them.
    #[must_use]
    pub fn multiplier_at(&self, model: &str, at: DateTime<Utc>) -> Option<f64> {
        let snapshot = snapshot_in_effect(&self.snapshots, at, |snapshot| snapshot.fetched)?;
        snapshot
            .models
            .get(model)
            .map(|multiplier| multiplier.on(at.date_naive()))
    }

    fn newest(&self) -> Option<&BTreeMap<String, Multiplier>> {
        self.snapshots.last().map(|snapshot| &snapshot.models)
    }
}

/// Prices Droid sessions at the model's first-party list price times its multiplier, both as of the session's start. A session whose model has no list price here, or no multiplier, is left unpriced.
pub fn price_droid_sessions(
    sessions: &mut [Session],
    prices: &PriceBook,
    multipliers: &MultiplierBook,
) {
    for session in sessions
        .iter_mut()
        .filter(|session| session.source == Source::Droid)
    {
        let Some(model) = session.model.as_deref() else {
            continue;
        };
        let (Some(key), Some(multiplier)) = (
            list_key(model),
            multipliers.multiplier_at(model, session.started),
        ) else {
            continue;
        };
        let Some(rate) = prices.exact_rate_at(key, session.started) else {
            continue;
        };
        session.cost_usd = Some(cost_usd(&session.tokens, &rate) * multiplier);
    }
}

/// The `LiteLLM` key for a Droid model's list price. A fast variant uses its base model's price, since its multiplier carries the premium.
fn list_key(model: &str) -> Option<&'static str> {
    let base = model
        .strip_suffix("-ultrafast")
        .or_else(|| model.strip_suffix("-fast"))
        .unwrap_or(model);
    LIST_KEYS
        .iter()
        .find(|(id, _)| *id == base)
        .map(|(_, key)| *key)
}

/// Reads every snapshot in `dir`, and names the `.json` files it had to skip. A missing folder is an empty book.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be read.
pub fn read(dir: &Path) -> io::Result<Loaded<MultiplierBook>> {
    let (snapshots, skipped) = read_snapshots(dir, Snapshot::from_json)?;
    Ok(Loaded {
        book: MultiplierBook::new(snapshots),
        skipped,
    })
}

/// Reads every snapshot in `dir`. A missing folder is an empty book.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be read.
pub fn load(dir: &Path) -> io::Result<MultiplierBook> {
    read(dir).map(|loaded| loaded.book)
}

/// Saves `snapshot` in `dir` when any multiplier differs from the newest saved one. Nothing is written when they match.
///
/// # Errors
///
/// Returns an error when `dir` cannot be read or written.
pub fn save(dir: &Path, snapshot: &Snapshot) -> io::Result<Refresh> {
    if load(dir)?.newest() == Some(&snapshot.models) {
        return Ok(Refresh::Unchanged);
    }
    write_snapshot(dir, snapshot.fetched, &snapshot.to_json())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use chrono::TimeZone;
    use proptest::prelude::{prop, prop_assert, prop_assert_eq, proptest};

    use super::*;
    use crate::model::Tokens;
    use crate::prices::{Rate, Snapshot as PriceSnapshot};
    use crate::testing::{RECORDED_TABLE, ScratchDir};

    const PAGE: &str = "\
| Model | Model ID | Multiplier | Reasoning |
| --- | --- | --- | --- |
| GPT-5.6 Sol<sup>†</sup> | `gpt-5.6-sol` | 1.6× | `none`, `low` |
| GPT-5.6 Sol Fast<sup>†</sup> | `gpt-5.6-sol-fast` | 3.2× | `none` |
| GLM-5.2 | `glm-5.2` | 0.56× | `off` |

<Note>
<sup>†</sup> Promotional pricing. GPT-5.6 Sol bills at 1.6× through November 22, 2026, then returns to 2×.

<sup>†</sup> Promotional pricing. GPT-5.6 Sol Fast bills at 3.2× through November 22, 2026, then returns to 4×.
</Note>";

    fn at(month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, month, day, hour, 0, 0).unwrap()
    }

    fn page_snapshot(fetched: DateTime<Utc>) -> Snapshot {
        parse_page(PAGE, fetched).unwrap()
    }

    fn list_book(day: DateTime<Utc>, models: &[(&str, f64)]) -> PriceBook {
        let rates = models
            .iter()
            .map(|(key, input)| {
                let rate = Rate {
                    input: *input,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                };
                ((*key).to_owned(), rate)
            })
            .collect::<BTreeMap<_, _>>();
        PriceBook::new(vec![PriceSnapshot {
            fetched: day,
            rates,
        }])
    }

    fn droid(model: &str, started: DateTime<Utc>) -> Session {
        Session {
            source: Source::Droid,
            started,
            project: None,
            model: Some(model.to_owned()),
            tokens: Tokens {
                input: 1_000_000,
                ..Tokens::default()
            },
            cost_usd: None,
        }
    }

    #[test]
    fn page_rows_and_promotions_are_read() {
        let snapshot = page_snapshot(at(10, 12, 0));
        assert_eq!(
            snapshot.models.len(),
            3,
            "the models page yields exactly three model rows"
        );
        assert_eq!(
            snapshot.models["glm-5.2"].rate, 0.56,
            "glm-5.2 has the base rate listed on the page"
        );
        let promo = snapshot.models["gpt-5.6-sol"].promo.unwrap();
        assert_eq!(
            promo.through,
            NaiveDate::from_ymd_opt(2026, 11, 22).unwrap(),
            "the gpt-5.6-sol promotion ends on the last listed day"
        );
        assert_eq!(
            promo.after, 2.0,
            "the promotion returns to the 2x rate after it ends"
        );
        assert_eq!(
            snapshot.models["gpt-5.6-sol-fast"].promo.unwrap().after,
            4.0,
            "the fast variant keeps its own promotion return rate"
        );
    }

    #[test]
    fn promotion_ends_after_its_last_day() {
        let snapshot = page_snapshot(at(10, 12, 0));
        let book = MultiplierBook::new(vec![snapshot]);
        assert_eq!(
            book.multiplier_at("gpt-5.6-sol", at(11, 22, 23)),
            Some(1.6),
            "a promotion still applies during its final hour"
        );
        assert_eq!(
            book.multiplier_at("gpt-5.6-sol", at(11, 23, 0)),
            Some(2.0),
            "the rate reverts once the promotion's last day has passed"
        );
        assert_eq!(
            book.multiplier_at("glm-5.2", at(11, 23, 0)),
            Some(0.56),
            "a model with no promotion keeps its base rate"
        );
        assert_eq!(
            book.multiplier_at("unknown", at(11, 23, 0)),
            None,
            "a model missing from the book has no multiplier"
        );
    }

    #[test]
    fn page_errors_say_what_is_wrong() {
        assert_eq!(
            PageError::NoModels.to_string(),
            "models page has no model rows",
            "a page with no model rows gives a clear error message"
        );
        assert_eq!(
            PageError::Note("GPT note".to_owned()).to_string(),
            "models page has an unreadable promotion note: GPT note",
            "an unreadable promotion note error names the note text"
        );
        assert_eq!(
            PageError::DuplicateModel("gpt-5.6-sol".to_owned()).to_string(),
            "models page lists model gpt-5.6-sol more than once",
            "a repeated model error names the model id that repeats"
        );
    }

    #[test]
    fn a_bad_promotion_note_fails_the_whole_page() {
        let wrong_rate = PAGE.replace("1.6× | `none`", "1.5× | `none`");
        assert!(
            matches!(
                parse_page(&wrong_rate, at(10, 12, 0)),
                Err(PageError::Note(_))
            ),
            "a promotion note with a wrong rate rejects the whole page"
        );
        let no_row = PAGE.replace("GPT-5.6 Sol<sup>†</sup>", "Other");
        assert!(
            matches!(parse_page(&no_row, at(10, 12, 0)), Err(PageError::Note(_))),
            "a promotion note whose model row is missing rejects the page"
        );
        assert!(
            matches!(
                parse_page("no table here", at(10, 12, 0)),
                Err(PageError::NoModels)
            ),
            "a page with no table at all reports no model rows"
        );
    }

    #[test]
    fn a_model_id_listed_twice_fails_the_page() {
        let doubled = format!("{PAGE}\n| Other | `gpt-5.6-sol` | 3× | `none` |");
        assert!(
            matches!(
                parse_page(&doubled, at(10, 12, 0)),
                Err(PageError::DuplicateModel(id)) if id == "gpt-5.6-sol"
            ),
            "a model id listed twice fails the page naming that id"
        );
    }

    #[test]
    fn a_name_listed_twice_fails_only_when_a_note_names_it() {
        let other = format!("{PAGE}\n| Other | `other-id` | 3× | `none` |");
        assert!(
            parse_page(&other, at(10, 12, 0)).is_ok(),
            "a repeated model name alone does not fail the page"
        );
        let copied = format!("{PAGE}\n| GPT-5.6 Sol | `gpt-5.6-sol-copy` | 1.6× | `none` |");
        assert!(
            matches!(parse_page(&copied, at(10, 12, 0)), Err(PageError::Note(_))),
            "a repeated model name fails the page when a note names it"
        );
    }

    #[test]
    fn promotion_rates_must_be_finite_and_positive() {
        for bad in ["NaN×", "inf×", "0×"] {
            let bad_note = PAGE.replace("bills at 1.6×", &format!("bills at {bad}"));
            assert!(
                matches!(
                    parse_page(&bad_note, at(10, 12, 0)),
                    Err(PageError::Note(_))
                ),
                "accepted note rate {bad}"
            );
            let bad_after = PAGE.replace("returns to 2×", &format!("returns to {bad}"));
            assert!(
                matches!(
                    parse_page(&bad_after, at(10, 12, 0)),
                    Err(PageError::Note(_))
                ),
                "accepted note return rate {bad}"
            );
        }
    }

    #[test]
    fn promotion_rate_matches_by_value_not_spelling() {
        let spelled = PAGE.replace("| 1.6× |", "| 1.60× |");
        let snapshot = parse_page(&spelled, at(10, 12, 0)).unwrap();
        assert_eq!(
            snapshot.models["gpt-5.6-sol"].rate, 1.6,
            "a rate written with extra zeros still reads as the same value"
        );
        assert!(
            snapshot.models["gpt-5.6-sol"].promo.is_some(),
            "the promotion is still read when the rate is respelled"
        );
    }

    #[test]
    fn droid_sessions_cost_list_price_times_multiplier() {
        let prices = list_book(at(10, 1, 0), &[("gpt-5.6-sol", 2e-6)]);
        let multipliers = MultiplierBook::new(vec![page_snapshot(at(10, 1, 0))]);
        let mut sessions = [
            droid("gpt-5.6-sol", at(10, 2, 0)),
            droid("gpt-5.6-sol-fast", at(10, 2, 0)),
            droid("gpt-5.6-sol", at(11, 25, 0)),
        ];
        price_droid_sessions(&mut sessions, &prices, &multipliers);
        // One million input tokens at $2e-6 each, times the multiplier in effect.
        assert!(
            (sessions[0].cost_usd.unwrap() - 2.0 * 1.6).abs() < 1e-9,
            "the first session costs list price times the multiplier in effect"
        );
        assert!(
            (sessions[1].cost_usd.unwrap() - 2.0 * 3.2).abs() < 1e-9,
            "a fast-variant session is billed at its own multiplier"
        );
        assert!(
            (sessions[2].cost_usd.unwrap() - 2.0 * 2.0).abs() < 1e-9,
            "a session after the promotion ends is billed at the regular rate"
        );
    }

    #[test]
    fn droid_sessions_without_a_list_price_or_multiplier_stay_unpriced() {
        // Only a third-party key is present, so the first-party lookup must not match it.
        let prices = list_book(at(10, 1, 0), &[("azure/gpt-5.6-sol", 2e-6)]);
        let multipliers = MultiplierBook::new(vec![page_snapshot(at(10, 1, 0))]);
        let mut sessions = [
            droid("gpt-5.6-sol", at(10, 2, 0)),
            droid("glm-5.2", at(10, 2, 0)),
            droid("gemini-4-argon", at(10, 2, 0)),
        ];
        price_droid_sessions(&mut sessions, &prices, &multipliers);
        assert!(
            sessions.iter().all(|session| session.cost_usd.is_none()),
            "no session is priced without a first-party list price and multiplier"
        );
    }

    #[test]
    fn other_sources_are_left_alone() {
        let prices = list_book(at(10, 1, 0), &[("gpt-5.6-sol", 2e-6)]);
        let multipliers = MultiplierBook::new(vec![page_snapshot(at(10, 1, 0))]);
        let mut session = droid("gpt-5.6-sol", at(10, 2, 0));
        session.source = Source::Agy;
        price_droid_sessions(std::slice::from_mut(&mut session), &prices, &multipliers);
        assert_eq!(
            session.cost_usd, None,
            "a session from another source keeps no cost"
        );
    }

    #[test]
    fn list_keys_strip_fast_suffixes_only() {
        assert_eq!(
            list_key("gpt-6.1-sol-ultrafast"),
            Some("gpt-6.1-sol"),
            "an ultrafast suffix is stripped to the base list key"
        );
        assert_eq!(
            list_key("glm-5.2-fast"),
            Some("zai/glm-5.2"),
            "a fast suffix on glm maps to the zai-prefixed list key"
        );
        assert_eq!(
            list_key("glm-5.3-flash"),
            Some("zai/glm-5.3-flash"),
            "a flash model keeps its own name as its list key"
        );
        assert_eq!(
            list_key("deepseek-v4.1-flash"),
            None,
            "an unknown model has no list key"
        );
    }

    /// The models page as the live source served it on 2026-10-25, unchanged.
    const RECORDED_PAGE: &str = include_str!("../tests/recorded/models-page.md");

    #[test]
    fn every_list_key_is_on_the_recorded_page_and_in_the_recorded_table() {
        let when = at(10, 25, 4);
        let prices = PriceBook::new(vec![
            crate::prices::parse_table(RECORDED_TABLE, when).unwrap(),
        ]);
        let page = parse_page(RECORDED_PAGE, when).unwrap();
        let mut ids = BTreeSet::new();
        for (id, key) in LIST_KEYS {
            // `list_key` returns the first match, so a repeated ID would hide its second entry.
            assert!(ids.insert(*id), "{id} is listed twice");
            assert_eq!(
                list_key(id),
                Some(*key),
                "{id} maps to its expected list key"
            );
            assert!(
                page.models.contains_key(*id),
                "{id} is not on the models page"
            );
            assert!(
                prices.exact_rate_at(key, when).is_some(),
                "{key} is not in the price table"
            );
        }
    }

    #[test]
    fn recorded_models_page_keeps_rates_and_promotions() {
        let snapshot = parse_page(RECORDED_PAGE, at(10, 25, 4)).unwrap();
        let sol = snapshot.models["gpt-5.6-sol"];
        assert_eq!(
            sol.rate, 1.6,
            "gpt-5.6-sol keeps its base rate from the recorded page"
        );
        assert_eq!(
            sol.promo,
            Some(Promo {
                through: NaiveDate::from_ymd_opt(2026, 11, 22).unwrap(),
                after: 2.0,
            }),
            "the recorded promotion keeps its end date and return rate"
        );
        let flash = snapshot.models["gemini-3.8-flash"];
        assert_eq!(
            flash.rate, 0.3,
            "the gemini flash model keeps its recorded base rate"
        );
        assert_eq!(
            flash.promo.map(|promo| promo.through),
            NaiveDate::from_ymd_opt(2027, 1, 1),
            "the gemini flash promotion ends on the recorded date"
        );
        assert_eq!(
            snapshot.models["glm-5.2"].promo, None,
            "a model with no promotion on the page has none"
        );
    }

    #[test]
    fn recorded_table_and_page_price_a_droid_session() {
        let prices = PriceBook::new(vec![
            crate::prices::parse_table(RECORDED_TABLE, at(10, 25, 4)).unwrap(),
        ]);
        let multipliers =
            MultiplierBook::new(vec![parse_page(RECORDED_PAGE, at(10, 25, 4)).unwrap()]);
        let mut session = Session {
            source: Source::Droid,
            started: at(10, 25, 9),
            project: None,
            model: Some("gpt-5.6-sol".to_owned()),
            tokens: Tokens {
                input: 1_000,
                output: 500,
                cache_read: 2_000,
                cache_write: 0,
            },
            cost_usd: None,
        };
        price_droid_sessions(std::slice::from_mut(&mut session), &prices, &multipliers);

        // List price: 1000 × $4e-6 + 500 × $2e-5 + 2000 × $4e-7 = $0.0148. Droid bills 1.6× that.
        let cost = session.cost_usd.unwrap();
        assert!((cost - 0.02368).abs() < 1e-12, "cost was {cost}");
    }

    #[test]
    fn promotions_end_on_a_utc_day_not_a_local_one() {
        let book = MultiplierBook::new(vec![page_snapshot(at(10, 1, 0))]);
        // 00:30 on Nov 22 at UTC-5 is 05:30 UTC on Nov 22, so the promotion still applies.
        let during = parse_timestamp("2026-11-22T00:30:00-05:00").unwrap();
        // 20:00 on Nov 22 at UTC-5 is 01:00 UTC on Nov 23, so the promotion has ended.
        let after = parse_timestamp("2026-11-22T20:00:00-05:00").unwrap();
        assert_eq!(
            book.multiplier_at("gpt-5.6-sol", during),
            Some(1.6),
            "a time still on the last UTC day gets the promotion rate"
        );
        assert_eq!(
            book.multiplier_at("gpt-5.6-sol", after),
            Some(2.0),
            "a time on the next UTC day gets the regular return rate"
        );
    }

    #[test]
    fn a_models_page_applies_from_its_exact_instant_with_an_offset() {
        let changed = PAGE.replace("0.56×", "0.6×");
        // 00:00 UTC on Oct 12 is 19:00 on Oct 11 at UTC-5.
        let newer = parse_page(&changed, parse_timestamp("2026-10-12T00:00:00Z").unwrap()).unwrap();
        let book = MultiplierBook::new(vec![page_snapshot(at(10, 1, 0)), newer]);

        let at_instant = parse_timestamp("2026-10-11T19:00:00-05:00").unwrap();
        let just_before = parse_timestamp("2026-10-11T18:59:59-05:00").unwrap();
        assert_eq!(
            book.multiplier_at("glm-5.2", at_instant),
            Some(0.6),
            "the new rate applies from the exact instant it takes effect"
        );
        assert_eq!(
            book.multiplier_at("glm-5.2", just_before),
            Some(0.56),
            "the old rate still applies one second before the change"
        );
    }

    #[test]
    fn snapshots_are_saved_only_when_a_multiplier_changes() {
        let dir = ScratchDir::new("multipliers-save");
        let first = page_snapshot(at(10, 12, 0));
        assert_eq!(
            save(&dir, &first).unwrap(),
            Refresh::Updated,
            "the first snapshot is saved as a new update"
        );
        assert_eq!(
            save(&dir, &page_snapshot(at(10, 13, 0))).unwrap(),
            Refresh::Unchanged,
            "a snapshot with the same multipliers is not saved again"
        );

        let changed = PAGE.replace("0.56×", "0.6×");
        let second = parse_page(&changed, at(10, 14, 0)).unwrap();
        assert_eq!(
            save(&dir, &second).unwrap(),
            Refresh::Updated,
            "a changed multiplier is saved as a new update"
        );
        let book = load(&dir).unwrap();
        assert!(
            !book.is_empty(),
            "a book loaded from saved snapshots is not empty"
        );
        assert_eq!(
            book.multiplier_at("glm-5.2", at(10, 13, 0)),
            Some(0.56),
            "the earlier saved rate still applies before the change"
        );
        assert_eq!(
            book.multiplier_at("glm-5.2", at(10, 15, 0)),
            Some(0.6),
            "the changed rate applies after the change time"
        );
    }

    #[test]
    fn an_empty_book_is_empty() {
        assert!(
            MultiplierBook::default().is_empty(),
            "a new book with no snapshots is empty"
        );
    }

    #[test]
    fn unreadable_snapshots_are_named_and_the_rest_still_load() {
        let dir = ScratchDir::new("multipliers-skips");
        let _ = save(&dir, &page_snapshot(at(10, 12, 0))).unwrap();
        fs::write(dir.join("broken.json"), "[1, 2").unwrap();
        fs::write(dir.join("wrong-shape.json"), r#"{"fetched": "x"}"#).unwrap();

        let loaded = read(&dir).unwrap();
        assert_eq!(
            loaded.skipped,
            ["broken.json", "wrong-shape.json"],
            "unreadable snapshot files are reported by file name"
        );
        assert_eq!(
            loaded.book.multiplier_at("glm-5.2", at(10, 13, 0)),
            Some(0.56),
            "the readable snapshot still loads after bad ones are skipped"
        );
    }

    #[test]
    fn rows_need_a_positive_finite_multiplier() {
        assert_eq!(
            model_row("| X | `x` | 0.5× | `none` |"),
            Some(("X", "x", 0.5)),
            "a row with a positive finite multiplier is read"
        );
        assert_eq!(
            model_row("| X | `x` | 0× | `none` |"),
            None,
            "a row with a zero multiplier is rejected"
        );
        assert_eq!(
            model_row("| X | `x` | inf× | `none` |"),
            None,
            "a row with an infinite multiplier is rejected"
        );
        assert_eq!(
            model_row("| X | `x` | NaN× | `none` |"),
            None,
            "a row with a NaN multiplier is rejected"
        );
    }

    #[test]
    fn a_saved_snapshot_with_a_bad_multiplier_or_no_models_is_not_read() {
        let good = page_snapshot(at(10, 12, 0)).to_json();
        let mut zero = good.clone();
        zero["models"]["glm-5.2"]["rate"] = json!(0.0);
        assert_eq!(
            Snapshot::from_json(&zero),
            None,
            "a saved snapshot with a zero multiplier is not read"
        );
        let mut promo = good.clone();
        promo["models"]["gpt-5.6-sol"]["promo"]["after"] = json!(-2.0);
        assert_eq!(
            Snapshot::from_json(&promo),
            None,
            "a saved snapshot with a negative promotion rate is not read"
        );
        let mut empty = good;
        empty["models"] = json!({});
        assert_eq!(
            Snapshot::from_json(&empty),
            None,
            "a saved snapshot with no models is not read"
        );
    }

    #[test]
    fn snapshots_round_trip_through_json() {
        let original = page_snapshot(at(10, 12, 0));
        assert_eq!(
            Snapshot::from_json(&original.to_json()).unwrap(),
            original,
            "a snapshot survives a JSON round trip unchanged"
        );
    }

    proptest! {
        #[test]
        fn parse_page_never_panics(text in ".*") {
            let _ = parse_page(&text, at(10, 12, 0));
        }

        #[test]
        fn a_page_row_only_yields_positive_finite_multipliers(rate in "[-+0-9.eEinfNa]{0,8}") {
            let page = format!("| Model | `m` | {rate}× | `none` |");
            if let Ok(snapshot) = parse_page(&page, at(10, 12, 0)) {
                for multiplier in snapshot.models.values() {
                    prop_assert!(multiplier.rate.is_finite() && multiplier.rate > 0.0);
                }
            }
        }

        #[test]
        fn saved_snapshots_read_back_unchanged(
            fetched_secs in 1_700_000_000i64..1_900_000_000,
            models in prop::collection::btree_map(
                "[a-z][a-z0-9.-]{0,10}",
                (0.001f64..1000.0, prop::option::of((0i64..2000, 0.001f64..1000.0))),
                1..6,
            ),
        ) {
            let snapshot = Snapshot {
                fetched: DateTime::from_timestamp(fetched_secs, 0).unwrap(),
                models: models
                    .into_iter()
                    .map(|(id, (rate, promo))| {
                        let promo = promo.map(|(days, after)| Promo {
                            through: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
                                + chrono::TimeDelta::days(days),
                            after,
                        });
                        (id, Multiplier { rate, promo })
                    })
                    .collect(),
            };
            prop_assert_eq!(Snapshot::from_json(&snapshot.to_json()), Some(snapshot));
        }
    }
}
