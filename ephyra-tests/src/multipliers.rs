//! Structured generators, shrinkers, and metrics for `droid_usage::multipliers`.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use ephyra::{Gen, GenExt, Source, gens};
use jiff::Timestamp;
use jiff::civil::Date;

/// Binary option whose shrunk state (`No`, 2 chars) is strictly shorter in `Debug` than `Yes` (3 chars).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// Option disabled.
    No,
    /// Option enabled.
    Yes,
}

/// A single row in a synthetic Factory models Markdown table.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRowSpec {
    /// Display name before optional footnote markers.
    pub name: String,
    /// Model identifier wrapped in backticks.
    pub id: String,
    /// Whether to include a `<sup>†</sup>` marker on the row.
    pub has_footnote: bool,
    /// Positive finite multiplier rate.
    pub rate: f64,
    /// Optional `(through_date, post_promo_rate)` promotion specification.
    pub promo: Option<(Date, f64)>,
}

/// Valid-by-construction Factory models page with optional duplicate-name noise.
#[derive(Debug, Clone, PartialEq)]
pub struct MultiplierPageCase {
    /// Snapshot fetch timestamp.
    pub fetched: Timestamp,
    /// Primary model rows.
    pub rows: Vec<ModelRowSpec>,
    /// Whether to append an unnoted extra row sharing `rows[0].name`.
    pub extra_unnoted_duplicate_name: Toggle,
    /// Whether to include heading and blank separator lines.
    pub noise_lines: Toggle,
}

impl MultiplierPageCase {
    /// Renders the Markdown page accepted by `droid_usage::multipliers::parse_page`.
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines = Vec::new();
        if self.noise_lines == Toggle::Yes {
            lines.push("# Models".to_owned());
            lines.push(String::new());
        }
        lines.push("| Model | Model ID | Multiplier | Reasoning |".to_owned());
        lines.push("| --- | --- | --- | --- |".to_owned());
        for row in &self.rows {
            let fn_mark = if row.has_footnote || row.promo.is_some() {
                "<sup>†</sup>"
            } else {
                ""
            };
            lines.push(format!(
                "| {}{} | `{}` | {}× | `none` |",
                row.name, fn_mark, row.id, row.rate
            ));
        }
        if self.extra_unnoted_duplicate_name == Toggle::Yes
            && let Some(first) = self.rows.first()
        {
            lines.push(format!(
                "| {} | `{}-dup-name` | {}× | `none` |",
                first.name, first.id, first.rate
            ));
        }
        let promo_rows: Vec<&ModelRowSpec> =
            self.rows.iter().filter(|r| r.promo.is_some()).collect();
        if !promo_rows.is_empty() {
            lines.push(String::new());
            lines.push("<Note>".to_owned());
            for row in promo_rows {
                if let Some((through, after)) = row.promo {
                    let date_str = through.strftime("%B %d, %Y");
                    lines.push(format!(
                        "<sup>†</sup> Promotional pricing. {} bills at {}× through {date_str}, then returns to {after}×.",
                        row.name, row.rate
                    ));
                }
            }
            lines.push("</Note>".to_owned());
        }
        lines.join("\n")
    }
}

fn rate_gen() -> impl Gen<Value = f64> {
    gens::pick_from(vec![
        0.1, 0.25, 0.3, 0.56, 1.0, 1.5, 1.6, 2.0, 3.2, 4.0, 10.0,
    ])
}

fn date_gen() -> impl Gen<Value = Date> {
    gens::int(0i64..=900)
        .map(|days| Date::new(2025, 1, 1).expect("valid base date") + jiff::Span::new().days(days))
}

/// Predicate defining valid `MultiplierPageCase` inputs.
#[must_use]
#[ephyra::generator]
pub fn valid_multiplier_page_case(case: &MultiplierPageCase) -> bool {
    if case.rows.is_empty() {
        return false;
    }
    let mut ids = BTreeSet::new();
    let mut name_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &case.rows {
        if row.id.is_empty()
            || row.name.is_empty()
            || row.name.contains('|')
            || row.name.contains('<')
            || row.id.contains('`')
            || row.id.contains('|')
            || !(row.rate.is_finite() && row.rate > 0.0)
        {
            return false;
        }
        if !ids.insert(row.id.as_str()) {
            return false;
        }
        *name_counts.entry(row.name.as_str()).or_default() += 1;
        if let Some((_, after)) = row.promo
            && !(after.is_finite() && after > 0.0)
        {
            return false;
        }
    }
    if case.extra_unnoted_duplicate_name == Toggle::Yes
        && let Some(first) = case.rows.first()
    {
        if first.promo.is_some() {
            return false;
        }
        let dup_id = format!("{}-dup-name", first.id);
        if case.rows.iter().any(|r| r.id == dup_id) {
            return false;
        }
        if case
            .rows
            .iter()
            .any(|r| r.name == first.name && r.promo.is_some())
        {
            return false;
        }
    }
    // Any row with a promo must have a unique plain display name.
    case.rows
        .iter()
        .all(|r| r.promo.is_none() || name_counts.get(r.name.as_str()).copied().unwrap_or(0) == 1)
}

fn draw_case(g: &mut Source) -> MultiplierPageCase {
    let n = g.int(1usize..=8);
    let base =
        Timestamp::new(1_760_000_000 + g.int(0i64..=30_000_000), 0).expect("valid timestamp");
    let mut rows = Vec::with_capacity(n);
    for idx in 0..n {
        let name = format!("Model-{idx}");
        let id = format!("model-{idx}");
        let rate = g.draw(&rate_gen());
        let promo = if g.bool_with(0.45) {
            Some((g.draw(&date_gen()), g.draw(&rate_gen())))
        } else {
            None
        };
        rows.push(ModelRowSpec {
            name,
            id,
            has_footnote: g.bool_with(0.25),
            rate,
            promo,
        });
    }
    let first_has_promo = rows[0].promo.is_some();
    MultiplierPageCase {
        fetched: base,
        rows,
        extra_unnoted_duplicate_name: if !first_has_promo && g.bool_with(0.3) {
            Toggle::Yes
        } else {
            Toggle::No
        },
        noise_lines: if g.bool() { Toggle::Yes } else { Toggle::No },
    }
}

fn shrink_case(
    case: &MultiplierPageCase,
    visit: &mut dyn FnMut(MultiplierPageCase) -> ControlFlow<()>,
) -> ControlFlow<()> {
    if case.extra_unnoted_duplicate_name == Toggle::Yes {
        let mut next = case.clone();
        next.extra_unnoted_duplicate_name = Toggle::No;
        visit(next)?;
    }
    if case.noise_lines == Toggle::Yes {
        let mut next = case.clone();
        next.noise_lines = Toggle::No;
        visit(next)?;
    }
    if case.rows.len() > 1 {
        for idx in (0..case.rows.len()).rev() {
            let mut removed = case.clone();
            let _ = removed.rows.remove(idx);
            if valid_multiplier_page_case(&removed) {
                visit(removed)?;
            }
        }
    }
    for (idx, row) in case.rows.iter().enumerate() {
        if row.promo.is_some() {
            let mut next = case.clone();
            next.rows[idx].promo = None;
            visit(next)?;
        }
    }
    ControlFlow::Continue(())
}

fn mutate_case(case: &MultiplierPageCase, g: &mut Source) -> MultiplierPageCase {
    let mut next = case.clone();
    match g.int(0u8..=2) {
        0 if !next.rows.is_empty() => {
            let idx = g.index(next.rows.len());
            next.rows[idx].rate = g.draw(&rate_gen());
        }
        1 => {
            next.noise_lines = match next.noise_lines {
                Toggle::Yes => Toggle::No,
                Toggle::No => Toggle::Yes,
            };
        }
        _ => {
            let idx = g.index(next.rows.len());
            if next.rows[idx].promo.is_some() {
                next.rows[idx].promo = None;
            } else if idx == 0 && next.extra_unnoted_duplicate_name == Toggle::Yes {
                next.extra_unnoted_duplicate_name = Toggle::No;
            } else {
                next.rows[idx].promo = Some((g.draw(&date_gen()), g.draw(&rate_gen())));
            }
        }
    }
    next
}

/// Metrics for `multiplier_pages`:
/// - `rows`: raw row count (higher tests table scaling).
/// - `promo_share`: fraction of rows carrying a promotion (0.0..=1.0).
#[must_use]
pub fn multiplier_page_metrics(
    case: &MultiplierPageCase,
    _size: usize,
) -> Vec<(&'static str, f64)> {
    let rows = f64::from(u32::try_from(case.rows.len()).unwrap_or(u32::MAX));
    let promos = f64::from(
        u32::try_from(case.rows.iter().filter(|r| r.promo.is_some()).count()).unwrap_or(u32::MAX),
    );
    vec![("rows", rows), ("promo_share", promos / rows.max(1.0))]
}

/// Generator of valid Factory multiplier Markdown pages.
#[ephyra::generator(valid_multiplier_page_case, metrics = multiplier_page_metrics)]
pub fn multiplier_pages() -> impl Gen<Value = MultiplierPageCase> {
    draw_case.with_shrink(shrink_case).with_mutate(mutate_case)
}
