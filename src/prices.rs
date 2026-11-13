//! Estimated session costs from list prices, kept as dated local snapshots.
//!
//! The source is the `LiteLLM` price table, a community-maintained JSON file that tracks vendor pricing pages. A snapshot is the set of chat-model rates seen at one moment. It is written only when a rate changes. A session is priced with the snapshot in effect when it started, so a price change applies only to sessions that start after it was seen. Promotions that begin and end between two checks are never seen.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::{Map, Value, json};

use crate::model::{Session, Source, Tokens, count_f64, parse_timestamp};

/// The price table this module reads.
pub const SOURCE_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// How long a check stays fresh before the report downloads the table again.
const MAX_AGE_HOURS: i64 = 24;

/// File that records when the table was last checked, kept beside the snapshots.
const CHECKED_FILE: &str = "last-checked";

/// USD per token for each kind of token.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    /// Uncached prompt tokens.
    pub input: f64,
    /// Completion tokens.
    pub output: f64,
    /// Cached prompt tokens read.
    pub cache_read: f64,
    /// Prompt tokens written to cache.
    pub cache_write: f64,
}

impl Rate {
    /// True when every price is finite and not negative.
    fn is_valid(&self) -> bool {
        [self.input, self.output, self.cache_read, self.cache_write]
            .into_iter()
            .all(|price| price.is_finite() && price >= 0.0)
    }
}

/// Every chat-model rate in the table at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// When the table was fetched (UTC).
    pub fetched: DateTime<Utc>,
    /// Rates keyed by the table's model key, for example `openai/gpt-5.5`.
    pub rates: BTreeMap<String, Rate>,
}

/// Why a price table could not be used.
#[derive(Debug)]
pub enum PriceError {
    /// The text is not JSON.
    Json(serde_json::Error),
    /// The JSON does not have the expected shape.
    Shape(&'static str),
    /// A snapshot could not be read or written.
    Io(io::Error),
}

impl fmt::Display for PriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(err) => write!(f, "price table is not JSON: {err}"),
            Self::Shape(problem) => write!(f, "price table is malformed: {problem}"),
            Self::Io(err) => write!(f, "price snapshot: {err}"),
        }
    }
}

impl Error for PriceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Shape(_) => None,
        }
    }
}

impl From<io::Error> for PriceError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Reads a LiteLLM-format table into a snapshot. Keeps chat and responses models that have an input and output price.
///
/// Cache prices missing from an entry fall back to the input price, which overstates cost for that entry and never understates it.
///
/// # Errors
///
/// Returns an error when the text is not a JSON object or holds no usable rates.
pub fn parse_table(text: &str, fetched: DateTime<Utc>) -> Result<Snapshot, PriceError> {
    let Value::Object(entries) = serde_json::from_str(text).map_err(PriceError::Json)? else {
        return Err(PriceError::Shape("top level is not an object"));
    };
    let mut rates: BTreeMap<String, Rate> = BTreeMap::new();
    for (key, entry) in &entries {
        let Some(rate) = rate_of(entry) else {
            continue;
        };
        // A bad price is a bad download, so reject the whole table rather than drop one model.
        if !rate.is_valid() {
            return Err(PriceError::Shape("a price is negative or not finite"));
        }
        let _ = rates.insert(key.clone(), rate);
    }
    if rates.is_empty() {
        return Err(PriceError::Shape(
            "no chat model has an input and output price",
        ));
    }
    Ok(Snapshot { fetched, rates })
}

fn rate_of(entry: &Value) -> Option<Rate> {
    if !matches!(entry["mode"].as_str()?, "chat" | "responses") {
        return None;
    }
    let input = entry["input_cost_per_token"].as_f64()?;
    Some(Rate {
        input,
        output: entry["output_cost_per_token"].as_f64()?,
        cache_read: entry["cache_read_input_token_cost"]
            .as_f64()
            .unwrap_or(input),
        cache_write: entry["cache_creation_input_token_cost"]
            .as_f64()
            .unwrap_or(input),
    })
}

impl Snapshot {
    /// The snapshot as stored on disk.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let rates: Map<String, Value> = self
            .rates
            .iter()
            .map(|(key, rate)| {
                (
                    key.clone(),
                    json!({
                        "input": rate.input,
                        "output": rate.output,
                        "cache_read": rate.cache_read,
                        "cache_write": rate.cache_write,
                    }),
                )
            })
            .collect();
        json!({
            "fetched": self.fetched.to_rfc3339_opts(SecondsFormat::Secs, true),
            "rates": rates,
        })
    }

    /// Reads a snapshot back from the form `to_json` writes. Returns `None` when a field is missing or a price is invalid.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        let fetched = parse_timestamp(value["fetched"].as_str()?)?;
        let rates = value["rates"]
            .as_object()?
            .iter()
            .map(|(key, rate)| {
                let rate = Rate {
                    input: rate["input"].as_f64()?,
                    output: rate["output"].as_f64()?,
                    cache_read: rate["cache_read"].as_f64()?,
                    cache_write: rate["cache_write"].as_f64()?,
                };
                rate.is_valid().then(|| (key.clone(), rate))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        (!rates.is_empty()).then_some(Self { fetched, rates })
    }
}

/// All saved snapshots, oldest first.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PriceBook {
    snapshots: Vec<Snapshot>,
}

impl PriceBook {
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

    /// The rate for `model` at time `at`. Uses the snapshot in effect at `at`, or the earliest snapshot when `at` comes before all of them.
    #[must_use]
    pub fn rate_at(&self, model: &str, at: DateTime<Utc>) -> Option<Rate> {
        let snapshot = snapshot_in_effect(&self.snapshots, at, |snapshot| snapshot.fetched)?;
        lookup(&snapshot.rates, model).copied()
    }

    /// The rate under exactly the key `key` at time `at`. Unlike `rate_at`, it never falls back to another provider's key.
    #[must_use]
    pub fn exact_rate_at(&self, key: &str, at: DateTime<Utc>) -> Option<Rate> {
        let snapshot = snapshot_in_effect(&self.snapshots, at, |snapshot| snapshot.fetched)?;
        snapshot.rates.get(key).copied()
    }

    fn newest_rates(&self) -> Option<&BTreeMap<String, Rate>> {
        self.snapshots.last().map(|snapshot| &snapshot.rates)
    }
}

/// The rate for a model name: the exact key, else the first `provider/<name>` key in name order. Several providers list the same model at different prices, so the choice of provider is a shortcut. Regional keys such as `azure/us/<name>` are not matched.
fn lookup<'a>(rates: &'a BTreeMap<String, Rate>, model: &str) -> Option<&'a Rate> {
    rates.get(model).or_else(|| {
        rates
            .iter()
            .find(|(key, _)| key.split_once('/').is_some_and(|(_, name)| name == model))
            .map(|(_, rate)| rate)
    })
}

/// Estimated USD cost of `tokens` at `rate`.
#[must_use]
pub fn cost_usd(tokens: &Tokens, rate: &Rate) -> f64 {
    count_f64(tokens.input) * rate.input
        + count_f64(tokens.output) * rate.output
        + count_f64(tokens.cache_read) * rate.cache_read
        + count_f64(tokens.cache_write) * rate.cache_write
}

/// Fills in `cost_usd` for sessions that have no recorded cost and a model the book prices. Sessions with a recorded cost keep it. Droid sessions are left out: they are priced by `multipliers::price_droid_sessions`.
pub fn price_sessions(sessions: &mut [Session], book: &PriceBook) {
    for session in sessions
        .iter_mut()
        .filter(|session| session.cost_usd.is_none() && session.source != Source::Droid)
    {
        let Some(model) = session.model.as_deref() else {
            continue;
        };
        if let Some(rate) = book.rate_at(model, session.started) {
            session.cost_usd = Some(cost_usd(&session.tokens, &rate));
        }
    }
}

/// A book read from a snapshot folder, plus the names of the `.json` files that could not be used.
#[derive(Debug)]
pub struct Loaded<B> {
    /// The book built from every usable snapshot.
    pub book: B,
    /// File names of `.json` files that could not be read or parsed. They were ignored.
    pub skipped: Vec<String>,
}

/// Reads every snapshot in `dir`, and names the `.json` files it had to skip. A missing folder is an empty book. Files that are not `.json` are ignored without a name.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be read.
pub fn read(dir: &Path) -> io::Result<Loaded<PriceBook>> {
    let (snapshots, skipped) = read_snapshots(dir, Snapshot::from_json)?;
    Ok(Loaded {
        book: PriceBook::new(snapshots),
        skipped,
    })
}

/// Reads every snapshot in `dir`. A missing folder is an empty book. Files that are not snapshots are ignored.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be read.
pub fn load(dir: &Path) -> io::Result<PriceBook> {
    read(dir).map(|loaded| loaded.book)
}

/// Reads every JSON file in `dir` that `parse` accepts, and returns the names of those it skipped. A missing folder holds none.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be read.
pub(crate) fn read_snapshots<T>(
    dir: &Path,
    parse: impl Fn(&Value) -> Option<T>,
) -> io::Result<(Vec<T>, Vec<String>)> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(err) => return Err(err),
    };
    let mut snapshots = Vec::new();
    let mut skipped = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let parsed = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| parse(&value));
        match parsed {
            Some(snapshot) => snapshots.push(snapshot),
            None => skipped.push(
                path.file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            ),
        }
    }
    skipped.sort();
    Ok((snapshots, skipped))
}

/// The snapshot in effect at `at`: the latest one fetched by then, or the earliest when `at` comes before all of them. `snapshots` must be sorted oldest first.
pub(crate) fn snapshot_in_effect<S>(
    snapshots: &[S],
    at: DateTime<Utc>,
    fetched: impl Fn(&S) -> DateTime<Utc>,
) -> Option<&S> {
    snapshots
        .iter()
        .rev()
        .find(|&snapshot| fetched(snapshot) <= at)
        .or_else(|| snapshots.first())
}

/// When the table was last checked, or `None` if it never was.
#[must_use]
pub fn last_checked(dir: &Path) -> Option<DateTime<Utc>> {
    let text = fs::read_to_string(dir.join(CHECKED_FILE)).ok()?;
    parse_timestamp(text.trim())
}

/// True when the table has not been checked within the last day.
#[must_use]
pub fn is_stale(dir: &Path, now: DateTime<Utc>) -> bool {
    last_checked(dir)
        .is_none_or(|checked| now.signed_duration_since(checked) > TimeDelta::hours(MAX_AGE_HOURS))
}

/// What a refresh did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Every rate matched the newest snapshot, so nothing was saved.
    Unchanged,
    /// At least one rate changed, so a new snapshot was saved.
    Updated,
}

/// Saves `snapshot` in `dir` when any rate differs from the newest saved one. Nothing is written when they match.
///
/// # Errors
///
/// Returns an error when `dir` cannot be read or written.
pub fn save(dir: &Path, snapshot: &Snapshot) -> io::Result<Refresh> {
    if load(dir)?.newest_rates() == Some(&snapshot.rates) {
        return Ok(Refresh::Unchanged);
    }
    write_snapshot(dir, snapshot.fetched, &snapshot.to_json())
}

/// Writes `json` into `dir` as a new snapshot named for `fetched`, creating `dir` first if needed. The snapshot appears in one step, so a crash never leaves a partial `.json` file for readers to parse.
///
/// # Errors
///
/// Returns an error when `dir` cannot be created or written.
pub(crate) fn write_snapshot(
    dir: &Path,
    fetched: DateTime<Utc>,
    json: &Value,
) -> io::Result<Refresh> {
    fs::create_dir_all(dir)?;
    let name = snapshot_file_name(fetched);
    // The temp name ends in `.tmp`, so `read_snapshots` never reads it.
    let temp = dir.join(format!(".{name}.tmp"));
    let result =
        write_synced(&temp, &json.to_string()).and_then(|()| fs::rename(&temp, dir.join(&name)));
    if result.is_err() {
        // Best effort: a leftover temp file is ignored by readers.
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(Refresh::Updated)
}

/// Writes `text` to `path` and flushes it to disk before returning.
fn write_synced(path: &Path, text: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    io::Write::write_all(&mut file, text.as_bytes())?;
    file.sync_all()
}

/// Removes the snapshot that `write_snapshot` wrote for `fetched`.
///
/// # Errors
///
/// Returns an error when the snapshot cannot be removed.
pub(crate) fn remove_snapshot(dir: &Path, fetched: DateTime<Utc>) -> io::Result<()> {
    fs::remove_file(dir.join(snapshot_file_name(fetched)))
}

fn snapshot_file_name(fetched: DateTime<Utc>) -> String {
    format!("{}.json", fetched.format("%Y%m%dT%H%M%SZ"))
}

/// Records `now` as the time the tables were last checked.
///
/// # Errors
///
/// Returns an error when `dir` cannot be written.
pub fn record_check(dir: &Path, now: DateTime<Utc>) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join(CHECKED_FILE),
        now.to_rfc3339_opts(SecondsFormat::Secs, true),
    )
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use proptest::prelude::{prop_assert, proptest};

    use super::*;
    use crate::testing::{RECORDED_TABLE, ScratchDir};

    /// Saves a parsed table and records the check, as one refresh does.
    fn refresh(dir: &Path, now: DateTime<Utc>, table: &str) -> Result<Refresh, PriceError> {
        let outcome = save(dir, &parse_table(table, now)?)?;
        record_check(dir, now)?;
        Ok(outcome)
    }

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, 12, 0, 0).unwrap()
    }

    fn rate(input: f64) -> Rate {
        Rate {
            input,
            output: input * 4.0,
            cache_read: input / 10.0,
            cache_write: input * 1.25,
        }
    }

    fn snapshot(day: u32, models: &[(&str, f64)]) -> Snapshot {
        Snapshot {
            fetched: at(day),
            rates: models
                .iter()
                .map(|(name, input)| ((*name).to_owned(), rate(*input)))
                .collect(),
        }
    }

    fn session(model: Option<&str>, day: u32, tokens: Tokens) -> Session {
        Session {
            source: Source::Agy,
            started: at(day),
            project: None,
            model: model.map(str::to_owned),
            tokens,
            cost_usd: None,
        }
    }

    const TABLE: &str = r#"{
        "sample_spec": {"mode": "chat", "input_cost_per_token": 9.0, "output_cost_per_token": 9.0},
        "openai/gpt-x": {
            "mode": "chat",
            "input_cost_per_token": 2e-06,
            "output_cost_per_token": 8e-06,
            "cache_read_input_token_cost": 2e-07
        },
        "text-embedding-x": {"mode": "embedding", "input_cost_per_token": 1e-07, "output_cost_per_token": 0.0},
        "no-prices": {"mode": "chat"}
    }"#;

    #[test]
    fn parse_keeps_chat_models_and_falls_back_to_input_for_cache_writes() {
        let parsed = parse_table(TABLE, at(10)).unwrap();
        assert_eq!(
            parsed.rates.keys().map(String::as_str).collect::<Vec<_>>(),
            ["openai/gpt-x", "sample_spec"],
            "only chat models are kept and embedding or priceless models are dropped"
        );
        let gpt = parsed.rates["openai/gpt-x"];
        assert_eq!(
            gpt.input, 2e-6,
            "the input price is read from the table as written"
        );
        assert_eq!(
            gpt.cache_read, 2e-7,
            "the cache read price is read from the table as written"
        );
        assert_eq!(
            gpt.cache_write, 2e-6,
            "a missing cache write price falls back to the input price"
        );
    }

    #[test]
    fn a_negative_price_fails_the_whole_table() {
        let input = r#"{"a":{"mode":"chat","input_cost_per_token":-1,"output_cost_per_token":2}}"#;
        assert!(
            matches!(parse_table(input, at(10)), Err(PriceError::Shape(_))),
            "a negative input price makes the whole table malformed"
        );
        let cache = r#"{"a":{"mode":"chat","input_cost_per_token":1,"output_cost_per_token":2,"cache_read_input_token_cost":-0.5}}"#;
        assert!(
            matches!(parse_table(cache, at(10)), Err(PriceError::Shape(_))),
            "a negative cache read price makes the whole table malformed"
        );
        let free = r#"{"a":{"mode":"chat","input_cost_per_token":0,"output_cost_per_token":0}}"#;
        assert_eq!(
            parse_table(free, at(10)).unwrap().rates["a"].input,
            0.0,
            "a zero price is accepted and kept as zero"
        );
    }

    #[test]
    fn a_rate_is_valid_only_when_every_price_is_finite_and_not_negative() {
        let priced = |price| Rate {
            input: price,
            output: 1.0,
            cache_read: 1.0,
            cache_write: 1.0,
        };
        assert!(priced(0.0).is_valid(), "a zero price makes a valid rate");
        assert!(
            !priced(-0.5).is_valid(),
            "a negative price makes a rate invalid"
        );
        assert!(
            !priced(f64::NAN).is_valid(),
            "a NaN price makes a rate invalid"
        );
        assert!(
            !priced(f64::INFINITY).is_valid(),
            "an infinite price makes a rate invalid"
        );
    }

    #[test]
    fn parse_rejects_non_objects_and_empty_tables() {
        assert!(
            matches!(parse_table("[]", at(10)), Err(PriceError::Shape(_))),
            "a top level array is rejected as a malformed table"
        );
        assert!(
            matches!(parse_table("{not json", at(10)), Err(PriceError::Json(_))),
            "text that is not JSON is rejected as a JSON error"
        );
        assert!(
            matches!(
                parse_table(r#"{"a":{"mode":"embedding"}}"#, at(10)),
                Err(PriceError::Shape(_))
            ),
            "a table with only embedding models is rejected as malformed"
        );
    }

    #[test]
    fn lookup_prefers_exact_keys_then_provider_prefixed_names() {
        let book = PriceBook::new(vec![snapshot(
            10,
            &[
                ("gpt-x", 1.0),
                ("openai/gpt-x", 2.0),
                ("azure/us/gpt-y", 3.0),
                ("azure/gpt-y", 4.0),
            ],
        )]);
        assert_eq!(
            book.rate_at("gpt-x", at(11)).unwrap().input,
            1.0,
            "an exact model key wins over a provider prefixed match"
        );
        assert_eq!(
            book.rate_at("gpt-y", at(11)).unwrap().input,
            4.0,
            "a bare name picks the one level provider prefix over deeper ones"
        );
        assert!(
            book.rate_at("missing", at(11)).is_none(),
            "a model name with no matching key has no rate"
        );
    }

    #[test]
    fn exact_rate_at_never_falls_back_to_another_provider() {
        let book = PriceBook::new(vec![snapshot(10, &[("azure/gpt-y", 4.0)])]);
        assert!(
            book.exact_rate_at("gpt-y", at(11)).is_none(),
            "an exact lookup does not match a provider prefixed key by bare name"
        );
        assert_eq!(
            book.exact_rate_at("azure/gpt-y", at(11)).unwrap().input,
            4.0,
            "an exact lookup finds the full provider prefixed key"
        );
    }

    #[test]
    fn rate_at_uses_the_snapshot_in_effect_at_that_time() {
        let book = PriceBook::new(vec![
            snapshot(20, &[("m", 2.0)]),
            snapshot(10, &[("m", 1.0)]),
        ]);
        assert_eq!(
            book.rate_at("m", at(5)).unwrap().input,
            1.0,
            "a time before every snapshot uses the earliest snapshot rate"
        );
        assert_eq!(
            book.rate_at("m", at(10)).unwrap().input,
            1.0,
            "the earliest snapshot still applies at its own fetch instant"
        );
        assert_eq!(
            book.rate_at("m", at(15)).unwrap().input,
            1.0,
            "between snapshots the older snapshot rate still applies"
        );
        assert_eq!(
            book.rate_at("m", at(20)).unwrap().input,
            2.0,
            "at the newer snapshot instant the newer rate applies"
        );
        assert_eq!(
            book.rate_at("m", at(28)).unwrap().input,
            2.0,
            "a time after the newer snapshot keeps using its rate"
        );
    }

    #[test]
    fn cost_is_tokens_times_rate_per_kind() {
        let tokens = Tokens {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_write: 0,
        };
        let r = Rate {
            input: 2e-6,
            output: 1e-5,
            cache_read: 2e-7,
            cache_write: 2.5e-6,
        };
        let expected = 2.0 + 5.0 + 0.4;
        assert!(
            (cost_usd(&tokens, &r) - expected).abs() < 1e-9,
            "cost is each token count times its own per token rate, summed"
        );
    }

    #[test]
    fn pricing_keeps_recorded_costs_and_skips_unknown_models() {
        let book = PriceBook::new(vec![snapshot(10, &[("m", 1.0)])]);
        let tokens = Tokens {
            input: 1_000_000,
            ..Tokens::default()
        };
        let mut sessions = [
            session(Some("m"), 11, tokens),
            session(Some("unknown"), 11, tokens),
            session(None, 11, tokens),
            Session {
                cost_usd: Some(0.0),
                ..session(Some("m"), 11, tokens)
            },
        ];
        price_sessions(&mut sessions, &book);
        // One million input tokens at $1 each.
        assert!(
            (sessions[0].cost_usd.unwrap() - 1_000_000.0).abs() < 1e-3,
            "a session cost is its token count times the model rate"
        );
        assert_eq!(
            sessions[1].cost_usd, None,
            "a session whose model is not in the book has no cost"
        );
        assert_eq!(
            sessions[2].cost_usd, None,
            "a session with no model has no cost"
        );
        assert_eq!(
            sessions[3].cost_usd,
            Some(0.0),
            "a recorded cost is kept even when the book has a rate"
        );
    }

    #[test]
    fn snapshots_round_trip_through_json() {
        let original = snapshot(10, &[("m", 1.5e-6)]);
        let back = Snapshot::from_json(&original.to_json()).unwrap();
        assert_eq!(
            back, original,
            "a snapshot survives a JSON round trip unchanged"
        );
    }

    #[test]
    fn a_saved_snapshot_with_a_bad_rate_or_no_rates_is_not_read() {
        let good = snapshot(10, &[("m", 1.0)]).to_json();
        let mut corrupt = good.clone();
        corrupt["rates"]["m"]["input"] = json!("bad");
        assert_eq!(
            Snapshot::from_json(&corrupt),
            None,
            "a snapshot with a non numeric rate is not read"
        );
        let mut negative = good.clone();
        negative["rates"]["m"]["output"] = json!(-1.0);
        assert_eq!(
            Snapshot::from_json(&negative),
            None,
            "a snapshot with a negative rate is not read"
        );
        let mut empty = good;
        empty["rates"] = json!({});
        assert_eq!(
            Snapshot::from_json(&empty),
            None,
            "a snapshot with no rates is not read"
        );
    }

    #[test]
    fn refresh_saves_only_changes_and_records_each_check() {
        let dir = ScratchDir::new("prices-refresh");
        assert!(
            is_stale(&dir, at(10)),
            "a folder with no recorded check yet is stale"
        );

        let first = refresh(&dir, at(10), TABLE).unwrap();
        assert_eq!(
            first,
            Refresh::Updated,
            "the first refresh stores the table and reports an update"
        );
        assert_eq!(
            last_checked(&dir),
            Some(at(10)),
            "the refresh instant is recorded as the last check time"
        );
        assert!(
            !is_stale(&dir, at(10) + TimeDelta::hours(1)),
            "a check one hour old is not yet stale"
        );
        assert!(
            is_stale(&dir, at(11) + TimeDelta::hours(1)),
            "a check more than a day old is stale"
        );

        let same = refresh(&dir, at(11), TABLE).unwrap();
        assert_eq!(
            same,
            Refresh::Unchanged,
            "a refresh with an identical table reports no change"
        );
        assert_eq!(
            load(&dir)
                .unwrap()
                .rate_at("openai/gpt-x", at(12))
                .unwrap()
                .input,
            2e-6,
            "an unchanged refresh keeps the original rate in the saved book"
        );

        let changed = TABLE.replace("2e-06", "1e-06");
        assert_eq!(
            refresh(&dir, at(12), &changed).unwrap(),
            Refresh::Updated,
            "a refresh with a changed rate reports an update"
        );
        let book = load(&dir).unwrap();
        assert_eq!(
            book.rate_at("openai/gpt-x", at(11)).unwrap().input,
            2e-6,
            "a time before the change still sees the old rate"
        );
        assert_eq!(
            book.rate_at("openai/gpt-x", at(13)).unwrap().input,
            1e-6,
            "a time after the change sees the new rate"
        );
    }

    #[test]
    fn recorded_table_gives_chat_rates_and_fills_cache_prices() {
        let rates = parse_table(RECORDED_TABLE, at(25)).unwrap().rates;
        // gpt-5.5 has no cache-write price in the table, so it falls back to the input price.
        assert_eq!(
            rates["gpt-5.5"],
            Rate {
                input: 5e-6,
                output: 3e-5,
                cache_read: 5e-7,
                cache_write: 5e-6,
            },
            "chat rates come from the table and a missing cache write price uses input"
        );
        // An explicit zero in the table stays zero.
        assert_eq!(
            rates["zai/glm-5.2"].cache_write, 0.0,
            "an explicit zero cache write price in the table stays zero"
        );
        assert!(
            !rates.contains_key("amazon.titan-embed-text-v1"),
            "embedding models are left out of the rates"
        );
    }

    #[test]
    fn a_price_snapshot_applies_from_its_exact_instant_with_an_offset() {
        let mut newer = snapshot(10, &[("m", 2.0)]);
        // 00:00 UTC on Oct 12 is 19:00 on Oct 11 at UTC-5.
        newer.fetched = parse_timestamp("2026-10-12T00:00:00Z").unwrap();
        let book = PriceBook::new(vec![snapshot(10, &[("m", 1.0)]), newer]);

        let at_instant = parse_timestamp("2026-10-11T19:00:00-05:00").unwrap();
        let just_before = parse_timestamp("2026-10-11T18:59:59-05:00").unwrap();
        assert_eq!(
            book.rate_at("m", at_instant).unwrap().input,
            2.0,
            "a snapshot applies from its exact instant even with a UTC offset"
        );
        assert_eq!(
            book.rate_at("m", just_before).unwrap().input,
            1.0,
            "one second before the instant the older snapshot still applies"
        );
    }

    #[test]
    fn missing_folder_is_an_empty_book() {
        let book = load(Path::new("/nonexistent-droid-usage-prices")).unwrap();
        assert!(
            book.is_empty(),
            "a missing snapshot folder loads as an empty book"
        );
    }

    #[test]
    fn unreadable_snapshots_are_named_and_the_rest_still_load() {
        let dir = ScratchDir::new("price-skips");
        let _ = save(&dir, &snapshot(10, &[("openai/gpt-x", 1.0)])).unwrap();
        fs::write(dir.join("broken.json"), "{not json").unwrap();
        fs::write(dir.join("wrong-shape.json"), r#"{"fetched": 1}"#).unwrap();
        fs::write(dir.join("notes.txt"), "ignored, not a snapshot").unwrap();

        let loaded = read(&dir).unwrap();
        assert_eq!(
            loaded.skipped,
            ["broken.json", "wrong-shape.json"],
            "unreadable snapshots are named as skipped and other files are ignored"
        );
        assert_eq!(
            loaded.book.rate_at("openai/gpt-x", at(11)).unwrap().input,
            1.0,
            "the good snapshot still loads after the bad ones are skipped"
        );
    }

    #[test]
    fn an_empty_book_is_empty_and_a_saved_one_is_not() {
        assert!(PriceBook::default().is_empty(), "a default book is empty");
        assert!(
            !PriceBook::new(vec![snapshot(10, &[("m", 1.0)])]).is_empty(),
            "a book holding a snapshot is not empty"
        );
    }

    #[test]
    fn price_errors_say_what_is_wrong() {
        let json = serde_json::from_str::<Value>("{").unwrap_err();
        assert!(
            PriceError::Json(json)
                .to_string()
                .starts_with("price table is not JSON: "),
            "a JSON price error message starts with the price table prefix"
        );
        assert_eq!(
            PriceError::Shape("empty").to_string(),
            "price table is malformed: empty",
            "a shape price error message names the malformed table and the detail"
        );
        assert_eq!(
            PriceError::Io(io::Error::other("disk")).to_string(),
            "price snapshot: disk",
            "an io price error message names the snapshot and the cause"
        );
    }

    #[test]
    fn a_snapshot_folder_that_is_a_file_is_an_error() {
        let path = ScratchDir::new("prices-file-not-dir");
        fs::write(&path, "not a folder").unwrap();
        assert!(
            load(&path).is_err(),
            "a snapshot path that is a file rather than a folder fails to load"
        );
    }

    #[test]
    fn a_saved_snapshot_leaves_only_its_json_file_behind() {
        let dir = ScratchDir::new("prices-no-temp");
        let outcome = save(&dir, &snapshot(10, &[("m", 1.0)])).unwrap();
        assert_eq!(
            outcome,
            Refresh::Updated,
            "the first save writes a snapshot"
        );
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["20261010T120000Z.json"],
            "the temp file used for the write is renamed away, not left in the folder"
        );
    }

    #[test]
    fn a_failed_snapshot_write_leaves_the_folder_as_it_was() {
        let dir = ScratchDir::new("prices-failed-write");
        // A folder of the snapshot's name cannot be replaced by the rename, so the write fails.
        fs::create_dir_all(dir.join("20261010T120000Z.json")).unwrap();
        let result = write_snapshot(&dir, at(10), &json!({"fetched": "x"}));
        assert!(
            result.is_err(),
            "a snapshot cannot replace a folder of the same name"
        );
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["20261010T120000Z.json"],
            "the failed write leaves no temp file behind"
        );
        assert!(
            dir.join("20261010T120000Z.json").is_dir(),
            "the existing folder is untouched"
        );
    }

    #[test]
    fn errors_report_their_underlying_cause() {
        let json = serde_json::from_str::<Value>("{").unwrap_err();
        assert!(
            PriceError::Json(json).source().is_some(),
            "a JSON price error exposes its underlying parse error as source"
        );
        assert!(
            PriceError::Io(io::Error::other("disk")).source().is_some(),
            "an io price error exposes its underlying cause as source"
        );
        assert!(
            PriceError::Shape("empty").source().is_none(),
            "a shape price error has no underlying source"
        );
    }

    #[test]
    fn staleness_starts_after_exactly_a_day() {
        let dir = ScratchDir::new("prices-stale");
        assert_eq!(
            refresh(&dir, at(10), TABLE).unwrap(),
            Refresh::Updated,
            "the first refresh of an empty folder stores the table"
        );
        let day = TimeDelta::hours(24);
        assert!(
            !is_stale(&dir, at(10) + day),
            "a check exactly one day old is not yet stale"
        );
        assert!(
            is_stale(&dir, at(10) + day + TimeDelta::seconds(1)),
            "a check one second over a day old is stale"
        );
    }

    proptest! {
        #[test]
        fn cost_is_additive_over_token_counts(
            a in 0u64..10_000_000,
            b in 0u64..10_000_000,
            rate_in in 0.0f64..1e-3,
        ) {
            let r = rate(rate_in);
            let one = cost_usd(&Tokens { input: a, ..Tokens::default() }, &r);
            let two = cost_usd(&Tokens { input: b, ..Tokens::default() }, &r);
            let both = cost_usd(&Tokens { input: a + b, ..Tokens::default() }, &r);
            prop_assert!((one + two - both).abs() <= both.abs() * 1e-12 + 1e-12);
        }

        #[test]
        fn parse_table_never_panics(text in ".*") {
            let _ = parse_table(&text, at(10));
        }
    }
}
