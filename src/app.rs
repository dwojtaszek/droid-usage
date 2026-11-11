//! The whole run: read the sources, refresh prices, price the sessions, and write the report.
//!
//! `main` supplies the real home folder, clock, terminal, and download. This module takes them as arguments, so tests can run the whole flow.

use std::error::Error;
use std::fmt::Display;
use std::io::{ErrorKind, Write};
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::download::Downloads;
use crate::model::{self, Session};
use crate::{agy, codex, droid, multipliers, opencode, prices, refresh, report};

/// Whether prices are refreshed before the report. `--offline` selects `Offline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Refresh the saved prices when they are older than a day.
    Online,
    /// Use the saved snapshots only.
    Offline,
}

/// Whether standard output is a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdout {
    /// A terminal, where colors show.
    Terminal,
    /// A file or a pipe.
    Redirected,
}

/// Whether the `NO_COLOR` variable is set. See <https://no-color.org>.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoColor {
    /// `NO_COLOR` is set.
    Set,
    /// `NO_COLOR` is not set.
    Unset,
}

/// Colors only for a terminal, and never when `NO_COLOR` is set.
#[must_use]
pub const fn use_color(stdout: Stdout, no_color: NoColor) -> report::Colors {
    match (stdout, no_color) {
        (Stdout::Terminal, NoColor::Unset) => report::Colors::On,
        (Stdout::Terminal, NoColor::Set) | (Stdout::Redirected, _) => report::Colors::Off,
    }
}

/// Runs the report. The report goes to `out`. Warnings go to `err`.
///
/// # Errors
///
/// Returns an error when a saved snapshot cannot be read or the report cannot be written. An unreadable source is only a warning.
pub fn run(
    home: &Path,
    mode: Mode,
    now: DateTime<Utc>,
    colors: report::Colors,
    download: impl FnOnce() -> Result<Downloads, Box<dyn Error>>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<(), Box<dyn Error>> {
    let data_dir = home.join(".local/share/droid-usage");
    let prices_dir = data_dir.join("prices");
    let multipliers_dir = data_dir.join("multipliers");

    let mut sessions: Vec<Session> = Vec::new();
    let source_warnings = [
        model::add_source(
            "droid",
            droid::read_sessions(&home.join(".factory/sessions")),
            &mut sessions,
        ),
        model::add_source(
            "opencode",
            opencode::read_sessions(&home.join(".local/share/opencode/opencode.db")),
            &mut sessions,
        ),
        model::add_source(
            "agy",
            agy::read_sessions(&home.join(".gemini/antigravity-cli")),
            &mut sessions,
        ),
        model::add_source(
            "codex",
            codex::read_sessions(&home.join(".codex")),
            &mut sessions,
        ),
    ];
    for warning in source_warnings.into_iter().flatten() {
        warn(err, warning);
    }

    let refresh_warning = match mode {
        Mode::Offline => None,
        Mode::Online => refresh::refresh_prices(&prices_dir, &multipliers_dir, now, download),
    };
    if let Some(warning) = refresh_warning {
        warn(err, warning);
    }

    let prices_read = prices::read(&prices_dir)?;
    for name in &prices_read.skipped {
        warn(err, format!("skipped unreadable price snapshot {name}"));
    }
    let book = prices_read.book;
    if book.is_empty() {
        warn(
            err,
            "no price snapshot saved; sessions without a recorded cost are left blank",
        );
    }
    let multipliers_read = multipliers::read(&multipliers_dir)?;
    for name in &multipliers_read.skipped {
        warn(
            err,
            format!("skipped unreadable multiplier snapshot {name}"),
        );
    }
    let multiplier_book = multipliers_read.book;
    if multiplier_book.is_empty() {
        warn(
            err,
            "no multiplier snapshot saved; Droid sessions are left blank",
        );
    }
    prices::price_sessions(&mut sessions, &book);
    multipliers::price_droid_sessions(&mut sessions, &book, &multiplier_book);

    // Exit quietly when the output is piped into something that stops reading, like `head`.
    match report::write_report(out, &sessions, colors) {
        Err(error) if error.kind() != ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(()),
    }
}

/// Writes a warning line. Failing to write one is not worth stopping for.
fn warn(err: &mut impl Write, message: impl Display) {
    let _ = writeln!(err, "{message}");
}

#[cfg(test)]
mod tests {
    use std::{fs, io};

    use super::*;
    use crate::refresh::apply;
    use crate::report::Colors;
    use crate::testing::{SAMPLE_PAGE, SAMPLE_TABLE, ScratchDir, no_download};

    fn at() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap()
    }

    /// A writer that fails every write with the given kind.
    struct Failing(ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn color_needs_a_terminal_and_no_no_color() {
        assert_eq!(
            use_color(Stdout::Terminal, NoColor::Unset),
            Colors::On,
            "colors are on only for a terminal when NO_COLOR is not set"
        );
        assert_eq!(
            use_color(Stdout::Terminal, NoColor::Set),
            Colors::Off,
            "setting NO_COLOR turns colors off even on a terminal"
        );
        assert_eq!(
            use_color(Stdout::Redirected, NoColor::Unset),
            Colors::Off,
            "colors stay off when stdout is redirected rather than a terminal"
        );
    }

    #[test]
    fn offline_run_with_nothing_saved_warns_and_still_reports() {
        let home = ScratchDir::new("app-empty");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            &home,
            Mode::Offline,
            at(),
            Colors::Off,
            no_download,
            &mut out,
            &mut err,
        )
        .unwrap();

        let out = String::from_utf8(out).unwrap();
        let err = String::from_utf8(err).unwrap();
        assert!(out.contains("By source"), "{out}");
        assert!(err.contains("no price snapshot saved"), "{err}");
        assert!(err.contains("no multiplier snapshot saved"), "{err}");
    }

    #[test]
    fn codex_sessions_appear_in_the_priced_report() {
        let home = ScratchDir::new("app-codex");
        let logs = home.join(".codex/sessions/2026/10/05");
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("rollout.jsonl"), concat!(
            r#"{"type":"session_meta","payload":{"timestamp":"2026-10-05T15:04:35Z","cwd":"/work/codex-app"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000000,"cached_input_tokens":0,"output_tokens":1000000}}}}"#,
        )).unwrap();
        apply(
            &home.join(".local/share/droid-usage/prices"),
            &home.join(".local/share/droid-usage/multipliers"),
            at(),
            SAMPLE_TABLE,
            SAMPLE_PAGE,
        )
        .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            &home,
            Mode::Offline,
            at(),
            Colors::Off,
            no_download,
            &mut out,
            &mut err,
        )
        .unwrap();
        let report = String::from_utf8(out).unwrap();
        assert!(report.contains("codex"), "{report}");
        assert!(report.contains("codex-app"), "{report}");
        assert!(report.contains("2000000"), "{report}");
        assert!(report.contains("10.00"), "{report}");
        assert!(!String::from_utf8(err).unwrap().contains("codex:"));
    }

    #[test]
    fn a_corrupt_snapshot_is_named_in_a_warning() {
        let home = ScratchDir::new("app-corrupt");
        let prices_dir = home.join(".local/share/droid-usage/prices");
        fs::create_dir_all(&prices_dir).unwrap();
        fs::write(prices_dir.join("20261001T000000Z.json"), "{truncated").unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            &home,
            Mode::Offline,
            at(),
            Colors::Off,
            no_download,
            &mut out,
            &mut err,
        )
        .unwrap();

        let err = String::from_utf8(err).unwrap();
        assert!(
            err.contains("skipped unreadable price snapshot 20261001T000000Z.json"),
            "{err}"
        );
    }

    #[test]
    fn a_failed_download_warns_and_the_report_still_runs() {
        let home = ScratchDir::new("app-failed");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let download = || Err("connection refused".into());
        run(
            &home,
            Mode::Online,
            at(),
            Colors::Off,
            download,
            &mut out,
            &mut err,
        )
        .unwrap();

        let err = String::from_utf8(err).unwrap();
        assert!(
            err.contains("price download failed, using saved snapshots: connection refused"),
            "{err}"
        );
        let report = String::from_utf8(out).unwrap();
        assert!(report.contains("By source"), "{report}");
    }

    #[test]
    fn a_successful_download_is_saved_before_the_prices_are_read() {
        let home = ScratchDir::new("app-downloaded");
        let download = || {
            Ok(Downloads {
                table: SAMPLE_TABLE.to_owned(),
                page: SAMPLE_PAGE.to_owned(),
            })
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            &home,
            Mode::Online,
            at(),
            Colors::Off,
            download,
            &mut out,
            &mut err,
        )
        .unwrap();

        let err = String::from_utf8(err).unwrap();
        assert!(!err.contains("no price snapshot saved"), "{err}");
        assert!(!err.contains("no multiplier snapshot saved"), "{err}");
        assert!(
            home.join(".local/share/droid-usage/prices").is_dir(),
            "a successful download creates the prices directory before prices are read"
        );
    }

    #[test]
    fn color_reaches_the_report() {
        let home = ScratchDir::new("app-color");
        let (mut colored, mut plain, mut err) = (Vec::new(), Vec::new(), Vec::new());
        run(
            &home,
            Mode::Offline,
            at(),
            Colors::On,
            no_download,
            &mut colored,
            &mut err,
        )
        .unwrap();
        run(
            &home,
            Mode::Offline,
            at(),
            Colors::Off,
            no_download,
            &mut plain,
            &mut err,
        )
        .unwrap();

        assert!(
            colored.contains(&0x1b),
            "colored report has no escape codes"
        );
        assert!(!plain.contains(&0x1b), "plain report has escape codes");
    }

    #[test]
    fn a_closed_pipe_is_not_an_error() {
        let home = ScratchDir::new("app-pipe");
        let mut out = Failing(ErrorKind::BrokenPipe);
        let mut err = Vec::new();
        assert!(
            run(
                &home,
                Mode::Offline,
                at(),
                Colors::Off,
                no_download,
                &mut out,
                &mut err
            )
            .is_ok(),
            "a broken pipe on stdout is ignored and the run succeeds"
        );
    }

    #[test]
    fn other_write_failures_are_errors() {
        let home = ScratchDir::new("app-write-fail");
        let mut out = Failing(ErrorKind::PermissionDenied);
        let mut err = Vec::new();
        assert!(
            run(
                &home,
                Mode::Offline,
                at(),
                Colors::Off,
                no_download,
                &mut out,
                &mut err
            )
            .is_err(),
            "a write failure other than a broken pipe is returned as an error"
        );
    }

    #[test]
    fn the_saved_check_time_is_not_touched_by_an_offline_run() {
        let home = ScratchDir::new("app-offline-keeps");
        let prices_dir = home.join(".local/share/droid-usage/prices");
        let multipliers_dir = home.join(".local/share/droid-usage/multipliers");
        apply(
            &prices_dir,
            &multipliers_dir,
            at(),
            SAMPLE_TABLE,
            SAMPLE_PAGE,
        )
        .unwrap();

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let later = at() + chrono::TimeDelta::days(3);
        run(
            &home,
            Mode::Offline,
            later,
            Colors::Off,
            no_download,
            &mut out,
            &mut err,
        )
        .unwrap();

        assert_eq!(
            prices::last_checked(&prices_dir),
            Some(at()),
            "an offline run does not change the recorded price check time"
        );
    }
}
