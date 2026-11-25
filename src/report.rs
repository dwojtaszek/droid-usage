//! Report: totals by source, weekly trend, models, and top projects, as terminal tables.

use std::io::{self, Write};

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};
use console::Style;
use unicode_width::UnicodeWidthStr;

use crate::model::{Session, count_f64};
use crate::summary::{Totals, all_totals, change_percent, group_by, ranked, weekly};

/// Rows in the Top projects table. A larger value makes only that table longer.
const TOP_PROJECTS: usize = 10;

/// Longest project name shown. Longer paths keep their start and end, so the project folder stays visible.
const PROJECT_WIDTH: usize = 40;

/// Background for every other data row, so wide tables are easy to follow.
const STRIPE: Color = Color::AnsiValue(236);

/// Whether the report is colored: bold headings, colored headers, and striped rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colors {
    /// Escape codes for color and bold are written.
    On,
    /// Plain text with no escape codes.
    Off,
}

/// A cost cell: the USD sum, or `-` when no session in the group has a cost.
fn money(totals: &Totals) -> String {
    if totals.priced == 0 {
        "-".to_owned()
    } else {
        format!("{:.2}", totals.cost_usd)
    }
}

/// The cost per million tokens over the priced sessions, or `-` when none of them has tokens.
fn per_million(totals: &Totals) -> String {
    if totals.priced_tokens == 0 {
        "-".to_owned()
    } else {
        format!(
            "{:.3}",
            totals.cost_usd * 1e6 / count_f64(totals.priced_tokens)
        )
    }
}

/// Rows for the Models table: the summary cells plus the cost per million tokens.
fn model_rows(sessions: &[Session]) -> Vec<Vec<String>> {
    ranked(group_by(sessions, |s| s.model.clone()))
        .into_iter()
        .map(|(model, totals)| {
            let label = model.unwrap_or_else(|| "(unknown)".to_owned());
            let mut cells = summary_cells(label, &totals);
            cells.push(per_million(&totals));
            cells
        })
        .collect()
}

/// Rows for the Top projects table: the busiest projects, at most `TOP_PROJECTS` of them.
fn project_rows(sessions: &[Session]) -> Vec<Vec<String>> {
    ranked(group_by(sessions, |s| s.project.clone()))
        .into_iter()
        .take(TOP_PROJECTS)
        .map(|(project, totals)| {
            let label = project.unwrap_or_else(|| "(unknown)".to_owned());
            summary_cells(shorten(&label, PROJECT_WIDTH), &totals)
        })
        .collect()
}

/// Cells for a group in the summary tables: label, sessions, with usage, tokens, cost.
fn summary_cells(label: String, totals: &Totals) -> Vec<String> {
    vec![
        label,
        totals.sessions.to_string(),
        totals.with_usage.to_string(),
        totals.tokens.total().to_string(),
        money(totals),
    ]
}

/// Cells for one row of the weekly table. `change` is the percent change from the week before.
fn weekly_cells(label: &str, totals: &Totals, change: &str) -> Vec<String> {
    vec![
        label.to_owned(),
        totals.sessions.to_string(),
        totals.with_usage.to_string(),
        totals.tokens.input.to_string(),
        totals.tokens.output.to_string(),
        totals.tokens.cache_read.to_string(),
        totals.tokens.total().to_string(),
        money(totals),
        change.to_owned(),
    ]
}

/// One titled table. `total`, when present, is drawn as a bold last row.
struct Section {
    title: &'static str,
    header: &'static [&'static str],
    /// Columns from this index on hold numbers and are right-aligned.
    numeric_from: usize,
    rows: Vec<Vec<String>>,
    total: Option<Vec<String>>,
}

/// How a row is drawn: a striped body row, or the total row.
#[derive(Clone, Copy)]
enum Kind {
    Body(usize),
    Total,
}

/// Styles a row's cells. Colors and bold are added only when `colors` is on. Alignment is always applied.
fn styled_row(cells: Vec<String>, numeric_from: usize, kind: Kind, colors: Colors) -> Vec<Cell> {
    cells
        .into_iter()
        .enumerate()
        .map(|(column, text)| {
            let mut cell = Cell::new(text);
            if column >= numeric_from {
                cell = cell.set_alignment(CellAlignment::Right);
            }
            if colors == Colors::Off {
                return cell;
            }
            match kind {
                Kind::Total => cell.add_attribute(Attribute::Bold).fg(Color::Yellow),
                Kind::Body(index) if index % 2 == 1 => cell.bg(STRIPE),
                Kind::Body(_) => cell,
            }
        })
        .collect()
}

/// Cuts the middle of `text` to fit `max` terminal columns, marking the cut with `…`. Wide characters count as two columns. Text that already fits is returned unchanged.
#[expect(
    clippy::string_slice,
    reason = "indices come from char_indices and sit on UTF-8 char boundaries"
)]
fn shorten(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_owned();
    }
    let Some(kept) = max.checked_sub(1) else {
        return String::new();
    };
    let head_budget = kept / 3;
    let tail_budget = kept - head_budget;
    // Each piece is measured as a string, the way the table measures the whole name. A character on its own can differ, such as a control character.
    let head_end = text
        .char_indices()
        .map(|(index, ch)| index + ch.len_utf8())
        .take_while(|&end| text[..end].width() <= head_budget)
        .last()
        .unwrap_or(0);
    let tail_start = text
        .char_indices()
        .rev()
        .map(|(index, _)| index)
        .take_while(|&start| text[start..].width() <= tail_budget)
        .last()
        .unwrap_or(text.len());
    format!("{}…{}", &text[..head_end], &text[tail_start..])
}

/// Writes a heading, the table, and a blank line.
fn write_section(out: &mut impl Write, section: Section, colors: Colors) -> io::Result<()> {
    let color = colors == Colors::On;
    writeln!(
        out,
        "{}",
        Style::new()
            .bold()
            .cyan()
            .force_styling(color)
            .apply_to(section.title)
    )?;
    let mut table = Table::new();
    // Table's builder methods return a reference that is not needed here.
    let _ = table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Disabled);
    if color {
        let _ = table.enforce_styling();
    }
    let header: Vec<Cell> = section
        .header
        .iter()
        .enumerate()
        .map(|(column, title)| {
            let cell = Cell::new(title);
            let cell = if column >= section.numeric_from {
                cell.set_alignment(CellAlignment::Right)
            } else {
                cell
            };
            if color {
                cell.add_attribute(Attribute::Bold).fg(Color::Cyan)
            } else {
                cell
            }
        })
        .collect();
    let _ = table.set_header(header);
    for (index, cells) in section.rows.into_iter().enumerate() {
        let _ = table.add_row(styled_row(
            cells,
            section.numeric_from,
            Kind::Body(index),
            colors,
        ));
    }
    if let Some(total) = section.total {
        let _ = table.add_row(styled_row(total, section.numeric_from, Kind::Total, colors));
    }
    writeln!(out, "{table}")?;
    writeln!(out)
}

/// Writes the full report for `sessions`. `colors` decides whether the report uses color.
///
/// # Errors
///
/// Returns an error when writing to `out` fails.
pub fn write_report(out: &mut impl Write, sessions: &[Session], colors: Colors) -> io::Result<()> {
    let sources = ranked(group_by(sessions, |s| s.source.to_string()))
        .into_iter()
        .map(|(name, totals)| summary_cells(name, &totals))
        .collect();
    write_section(
        out,
        Section {
            title: "By source",
            header: &["source", "sessions", "with usage", "tokens", "cost usd"],
            numeric_from: 1,
            rows: sources,
            total: None,
        },
        colors,
    )?;

    let weeks = weekly(sessions);
    let mut week_rows = Vec::new();
    for (index, (week, totals)) in weeks.iter().enumerate() {
        let change = index
            .checked_sub(1)
            .and_then(|prev| change_percent(weeks[prev].1.tokens.total(), totals.tokens.total()))
            .map_or_else(|| "-".to_owned(), |pct| format!("{pct:+.1}%"));
        week_rows.push(weekly_cells(&week.to_string(), totals, &change));
    }
    let all = all_totals(sessions);
    let total = (!weeks.is_empty()).then(|| weekly_cells("total", &all, "-"));
    write_section(
        out,
        Section {
            title: "Weekly (weeks start Monday, UTC)",
            header: &[
                "week",
                "sessions",
                "with usage",
                "input",
                "output",
                "cache read",
                "tokens",
                "cost usd",
                "change",
            ],
            numeric_from: 1,
            rows: week_rows,
            total,
        },
        colors,
    )?;

    write_section(
        out,
        Section {
            title: "Models",
            header: &[
                "model",
                "sessions",
                "with usage",
                "tokens",
                "cost usd",
                "cost per 1M tokens",
            ],
            numeric_from: 1,
            rows: model_rows(sessions),
            total: None,
        },
        colors,
    )?;

    let projects = project_rows(sessions);
    write_section(
        out,
        Section {
            title: "Top projects",
            header: &["project", "sessions", "with usage", "tokens", "cost usd"],
            numeric_from: 1,
            rows: projects,
            total: None,
        },
        colors,
    )?;

    if all.unpriced > 0 {
        let note = format!(
            "Note: {} session(s) with usage have no cost, because the model is unknown or not in the price table. Their tokens count; their cost does not.",
            all.unpriced
        );
        writeln!(
            out,
            "{}",
            Style::new()
                .dim()
                .force_styling(colors == Colors::On)
                .apply_to(note)
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use jiff::civil::Date;
    use proptest::prelude::{prop_assert, prop_assert_eq, proptest};

    use super::*;
    use crate::model::{Source, Tokens};

    fn session_on(day: Date, input: u64) -> Session {
        Session {
            source: Source::Droid,
            started: day
                .at(12, 0, 0, 0)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .unwrap()
                .timestamp(),
            project: None,
            model: None,
            tokens: Tokens {
                input,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            cost_usd: None,
        }
    }

    fn date(year: i16, month: i8, day: i8) -> Date {
        Date::new(year, month, day).unwrap()
    }

    /// Two weeks of sessions, the second one priced.
    fn two_weeks() -> [Session; 2] {
        [
            session_on(date(2026, 10, 5), 100),
            Session {
                cost_usd: Some(0.25),
                ..session_on(date(2026, 10, 12), 150)
            },
        ]
    }

    /// Cells of a table row, split on the column dividers (`┆` between columns, `│` at the edges).
    fn cells(line: &str) -> Vec<&str> {
        line.split(['│', '┆'])
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .collect()
    }

    /// Cells of the row whose first cell is `label`.
    fn cells_of<'a>(lines: &[&'a str], label: &str) -> Vec<&'a str> {
        lines
            .iter()
            .map(|line| cells(line))
            .find(|row| row.first() == Some(&label))
            .unwrap()
    }

    #[test]
    fn money_and_cost_per_million_tokens_format_and_skip_unpriced_sessions() {
        let priced = Session {
            cost_usd: Some(2.0),
            ..session_on(date(2026, 10, 5), 1_000_000)
        };
        let unpriced = session_on(date(2026, 10, 6), 9_000_000);
        let mut totals = Totals::default();
        for session in [&priced, &unpriced] {
            totals.add_session(session);
        }
        assert_eq!(money(&totals), "2.00");
        assert_eq!(money(&Totals::default()), "-");
        assert_eq!(
            per_million(&totals),
            "2.000",
            "cost per million tokens leaves out unpriced session tokens"
        );
        assert_eq!(
            per_million(&Totals::default()),
            "-",
            "cost per million shows a dash when there are no tokens"
        );
    }

    #[test]
    fn report_shows_week_change_costs_and_unknown_labels() {
        let mut out = Vec::new();
        write_report(&mut out, &two_weeks(), Colors::Off).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("By source"),
            "the report has a by source section"
        );
        assert!(text.contains("Weekly"), "the report has a weekly section");
        assert!(text.contains("Models"), "the report has a models section");
        assert!(
            text.contains("Top projects"),
            "the report has a top projects section"
        );
        assert!(
            text.contains("cost per 1M tokens"),
            "the report has a cost per 1M tokens column"
        );
        assert!(
            text.contains("+50.0%"),
            "the report shows the week over week change"
        );
        assert!(
            text.contains("(unknown)"),
            "sessions with no model or project are labelled unknown"
        );
        assert!(
            text.contains("with usage"),
            "the report has a with usage column"
        );
        assert!(
            text.contains("cost usd"),
            "the report has a cost usd column"
        );
        assert!(
            text.contains("0.25"),
            "the report shows the summed cost of 0.25"
        );
        assert!(
            text.contains("Note: 1 session(s) with usage have no cost"),
            "the report warns how many usage sessions lack a cost"
        );
    }

    #[test]
    fn models_table_has_one_row_per_model_with_its_cost_per_million() {
        let priced = Session {
            model: Some("gpt-x".to_owned()),
            cost_usd: Some(2.0),
            ..session_on(date(2026, 10, 5), 1_000_000)
        };
        let mut out = Vec::new();
        write_report(&mut out, &[priced], Colors::Off).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            cells_of(&lines, "gpt-x"),
            ["gpt-x", "1", "1", "1000000", "2.00", "2.000"],
            "the model row shows counts, tokens, cost and cost per million"
        );
    }

    #[test]
    fn a_model_named_unknown_is_a_separate_row_from_a_missing_model() {
        let missing = session_on(date(2026, 10, 5), 10);
        let named = Session {
            model: Some("(unknown)".to_owned()),
            project: Some("(unknown)".to_owned()),
            ..session_on(date(2026, 10, 6), 20)
        };
        let sessions = [missing, named];
        let models = model_rows(&sessions);
        assert_eq!(models.len(), 2, "{models:?}");
        assert!(models.iter().all(|row| row[1] == "1"), "{models:?}");
        let projects = project_rows(&sessions);
        assert_eq!(projects.len(), 2, "{projects:?}");
        assert!(projects.iter().all(|row| row[1] == "1"), "{projects:?}");
    }

    #[test]
    fn top_projects_keeps_the_busiest_ten() {
        let mut sessions = Vec::new();
        for project in 0..11 {
            for _ in 0..(11 - project) {
                sessions.push(Session {
                    project: Some(format!("/work/p{project:02}")),
                    ..session_on(date(2026, 10, 5), 1)
                });
            }
        }
        let rows = project_rows(&sessions);
        assert_eq!(
            rows.len(),
            TOP_PROJECTS,
            "only the ten busiest projects get a row"
        );
        assert_eq!(
            rows[0][0], "/work/p00",
            "the busiest project is listed first"
        );
        assert!(rows.iter().all(|row| row[0] != "/work/p10"), "{rows:?}");
    }

    #[test]
    fn the_cost_note_appears_only_when_a_session_is_unpriced() {
        let priced = Session {
            cost_usd: Some(1.0),
            ..session_on(date(2026, 10, 5), 10)
        };
        let mut out = Vec::new();
        write_report(&mut out, &[priced], Colors::Off).unwrap();
        assert!(
            !String::from_utf8(out).unwrap().contains("Note:"),
            "no cost note when every session is priced"
        );
    }

    #[test]
    fn weekly_table_ends_with_a_total_row() {
        let mut out = Vec::new();
        write_report(&mut out, &two_weeks(), Colors::Off).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let cells = cells_of(&lines, "total");
        assert_eq!(
            cells,
            ["total", "2", "2", "250", "0", "0", "250", "0.25", "-"],
            "the total row sums the weekly columns"
        );
        let last_week = lines
            .iter()
            .position(|line| line.contains("2026-10-12"))
            .unwrap();
        let total = lines
            .iter()
            .position(|line| line.contains("total"))
            .unwrap();
        assert_eq!(
            total,
            last_week + 1,
            "the total row comes right after the last week row"
        );
    }

    #[test]
    fn shorten_keeps_the_start_and_end_of_long_names() {
        assert_eq!(
            shorten("/tmp/app", 40),
            "/tmp/app",
            "a name that fits the width is returned unchanged"
        );
        let long = "/home/dom/orca/workspaces/zipper/replace-cli-parse-validate";
        let short = shorten(long, 40);
        assert_eq!(
            short.chars().count(),
            40,
            "a long name is cut to exactly the requested width"
        );
        assert!(
            short.starts_with("/home/dom/orc"),
            "a cut name keeps the start of the path"
        );
        assert!(
            short.ends_with("replace-cli-parse-validate"),
            "a cut name keeps the end of the path"
        );
        assert!(
            short.contains('…'),
            "a cut name shows an ellipsis where text was removed"
        );
    }

    #[test]
    fn shorten_limits_display_columns_not_characters() {
        // Each CJK character is two columns wide. Long enough that the cut must happen.
        let wide =
            "仕事のプロジェクトデータ分析レポート生成パイプラインの再構築作業場所テスト".repeat(2);
        let short = shorten(&wide, 40);
        assert!(
            short.width() <= 40,
            "{short:?} is {} columns",
            short.width()
        );
        assert!(
            short.starts_with("仕事"),
            "a wide name keeps its start within the column limit"
        );
        assert!(
            short.ends_with("場所テスト"),
            "a wide name keeps its end within the column limit"
        );
        assert!(
            short.contains('…'),
            "a wide name that is cut shows an ellipsis"
        );
    }

    #[test]
    fn shorten_to_zero_columns_is_empty() {
        assert_eq!(shorten("abc", 0), "", "zero columns leave no text");
        assert_eq!(
            shorten("", 0),
            "",
            "an empty name stays empty at zero columns"
        );
    }

    #[test]
    fn shorten_counts_control_characters_as_the_table_does() {
        // A vertical tab takes a column in the table, but a single character has no width for it.
        let name = "0a \u{b}  0 A𠀀a¡Aa \u{b}A";
        let short = shorten(name, 2);
        assert!(short.width() <= 2, "{short:?} is {} columns", short.width());
        let tabbed = "src\tlib/main.rs with a tab in its name";
        let short = shorten(tabbed, 12);
        assert!(
            short.width() <= 12,
            "{short:?} is {} columns",
            short.width()
        );
    }

    #[test]
    fn wide_project_names_keep_the_table_within_the_width_budget() {
        let wide =
            "/home/dom/仕事/プロジェクト/データ分析/レポート生成パイプラインの再構築作業場所";
        let sessions = [Session {
            project: Some(wide.to_owned()),
            ..session_on(date(2026, 10, 5), 10)
        }];
        let mut out = Vec::new();
        write_report(&mut out, &sessions, Colors::Off).unwrap();
        let text = String::from_utf8(out).unwrap();
        let table: Vec<&str> = text
            .lines()
            .skip_while(|line| !line.contains("Top projects"))
            .skip(1)
            .take_while(|line| !line.is_empty())
            .collect();
        assert!(table.len() > 2, "{text}");
        let widths: Vec<usize> = table.iter().map(|line| line.width()).collect();
        assert!(widths.iter().all(|width| *width == widths[0]), "{widths:?}");
        assert!(widths[0] <= 100, "table is {} columns wide", widths[0]);
    }

    #[test]
    fn long_project_names_are_shortened_in_the_table() {
        let long = "/home/dom/orca/workspaces/zipper/replace-cli-parse-validate-build-pipeline";
        let sessions = [Session {
            project: Some(long.to_owned()),
            ..session_on(date(2026, 10, 5), 10)
        }];
        let mut out = Vec::new();
        write_report(&mut out, &sessions, Colors::Off).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains(long),
            "a long project name is not printed in full"
        );
        let table_lines = text
            .lines()
            .filter(|line| line.starts_with(['┌', '│', '╞', '└']));
        assert!(
            table_lines.clone().count() > 0,
            "the report prints at least one table line"
        );
        assert!(
            table_lines.clone().all(|line| line.chars().count() <= 100),
            "every table line fits within 100 characters"
        );
    }

    #[test]
    fn plain_report_has_no_escape_codes() {
        let mut out = Vec::new();
        write_report(&mut out, &two_weeks(), Colors::Off).unwrap();
        assert!(
            !String::from_utf8(out).unwrap().contains('\u{1b}'),
            "the plain report has no terminal escape codes"
        );
    }

    #[test]
    fn colored_report_bolds_headings_and_stripes_odd_rows() {
        let mut out = Vec::new();
        write_report(&mut out, &two_weeks(), Colors::On).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\u{1b}[1m"), "headings and totals are bold");
        let lines: Vec<&str> = text.lines().collect();
        let first = lines
            .iter()
            .find(|line| line.contains("2026-10-05"))
            .unwrap();
        let second = lines
            .iter()
            .find(|line| line.contains("2026-10-12"))
            .unwrap();
        assert!(!first.contains("48;5;236"), "the first body row is plain");
        assert!(
            second.contains("48;5;236"),
            "the second body row is striped"
        );
    }

    /// Cells of a table row with their padding kept, so alignment can be checked.
    fn raw_cells(line: &str) -> Vec<&str> {
        line.split(['│', '┆'])
            .filter(|cell| !cell.is_empty())
            .collect()
    }

    #[test]
    fn numbers_are_right_aligned_and_labels_left_aligned() {
        let mut out = Vec::new();
        write_report(&mut out, &two_weeks(), Colors::Off).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let header = lines.iter().find(|line| line.contains("│ week")).unwrap();
        assert!(
            raw_cells(header)[0].starts_with(" week "),
            "the week header is left aligned with a leading space"
        );
        let row = lines
            .iter()
            .find(|line| line.contains("2026-10-12"))
            .unwrap();
        assert_eq!(
            raw_cells(row)[1],
            "        1 ",
            "a count cell is right aligned in its column"
        );
    }

    proptest! {
        #[test]
        fn shortened_names_fit_their_column_and_short_ones_are_kept(
            // Mixes in control characters, joiners, and variation selectors, which change widths.
            text in "[a-z /\u{b}\t\u{200d}\u{fe0f}⚠👍🏻仕事…]{0,40}|.*",
            max in 1usize..80,
        ) {
            let short = shorten(&text, max);
            prop_assert!(short.width() <= max);
            if text.width() <= max {
                prop_assert_eq!(short, text);
            }
        }
    }
}
