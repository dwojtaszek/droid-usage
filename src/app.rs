//! The whole run: read the sources, refresh prices, price the sessions, and write the report.
//!
//! `main` supplies the real home folder, clock, terminal, and download. This module takes them as arguments, so tests can run the whole flow.

use std::error::Error;
use std::fmt::Display;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::download::Downloads;
use crate::model::{self, Session, Source};
use crate::snapshots::Loaded;
use crate::{agy, codex, droid, multipliers, opencode, pi, prices, pricing, refresh, report};

/// Whether prices are refreshed before the report. `--offline` selects `Offline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Refresh the saved prices when they are older than a day.
    Online,
    /// Use the saved snapshots only.
    Offline,
}

impl Mode {
    /// Pairs this CLI mode with a download callback for online runs.
    #[must_use]
    pub fn with_download<F>(self, download: F) -> SourceRefresh<F> {
        match self {
            Self::Offline => SourceRefresh::Offline,
            Self::Online => SourceRefresh::Online(download),
        }
    }
}

/// Whether and how saved price snapshots are refreshed for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRefresh<F> {
    /// Use the saved snapshots only.
    Offline,
    /// Download fresh snapshots with `F` when older than a day.
    Online(F),
}

impl SourceRefresh<fn() -> Result<Downloads, Box<dyn Error>>> {
    /// Offline mode without needing a dummy download callback.
    #[must_use]
    pub const fn offline() -> Self {
        Self::Offline
    }
}

struct SnapshotDirs {
    prices: PathBuf,
    multipliers: PathBuf,
}

impl SnapshotDirs {
    fn under(home: &Path) -> Self {
        let data = home.join(".local/share/droid-usage");
        Self {
            prices: data.join("prices"),
            multipliers: data.join("multipliers"),
        }
    }
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

type SourceReader = fn(&Path) -> Result<model::Read, String>;

/// Runs the report. The report goes to `out`. Warnings go to `err`.
///
/// # Errors
///
/// Returns an error when a saved snapshot cannot be read or the report cannot be written. An unreadable source is only a warning.
pub fn run<F>(
    home: &Path,
    refresh_mode: SourceRefresh<F>,
    now: DateTime<Utc>,
    colors: report::Colors,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<(), Box<dyn Error>>
where
    F: FnOnce() -> Result<Downloads, Box<dyn Error>>,
{
    let dirs = SnapshotDirs::under(home);

    let mut sessions: Vec<Session> = Vec::new();
    let readers: [(Source, &str, SourceReader); 5] = [
        (Source::Droid, ".factory/sessions", |path| {
            droid::read_sessions(path).map_err(|error| error.to_string())
        }),
        (
            Source::OpenCode,
            ".local/share/opencode/opencode.db",
            |path| opencode::read_sessions(path).map_err(|error| error.to_string()),
        ),
        (Source::Agy, ".gemini/antigravity-cli", |path| {
            agy::read_sessions(path).map_err(|error| error.to_string())
        }),
        (Source::Codex, ".codex", |path| {
            codex::read_sessions(path).map_err(|error| error.to_string())
        }),
        (Source::Pi, ".pi/agent/sessions", |path| {
            pi::read_sessions(path).map_err(|error| error.to_string())
        }),
    ];
    for (source, rel_path, read) in readers {
        if let Some(warning) =
            model::add_source(source.as_str(), read(&home.join(rel_path)), &mut sessions)
        {
            warn(err, warning);
        }
    }

    if let SourceRefresh::Online(download) = refresh_mode
        && let Some(warning) =
            refresh::refresh_prices(&dirs.prices, &dirs.multipliers, now, download)
    {
        warn(err, warning);
    }

    let book = read_book(
        err,
        "price",
        "no price snapshot saved; sessions without a recorded cost are left blank",
        prices::read(&dirs.prices),
        prices::PriceBook::is_empty,
    )?;
    let multiplier_book = read_book(
        err,
        "multiplier",
        "no multiplier snapshot saved; Droid sessions are left blank",
        multipliers::read(&dirs.multipliers),
        multipliers::MultiplierBook::is_empty,
    )?;
    pricing::price_sessions(&mut sessions, &book, &multiplier_book);

    // Exit quietly when the output is piped into something that stops reading, like `head`.
    match report::write_report(out, &sessions, colors) {
        Err(error) if error.kind() != ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(()),
    }
}

fn read_book<B>(
    err: &mut impl Write,
    kind: &str,
    empty_note: &str,
    read: io::Result<Loaded<B>>,
    is_empty: impl FnOnce(&B) -> bool,
) -> io::Result<B> {
    let loaded = read?;
    for name in &loaded.skipped {
        warn(err, format!("skipped unreadable {kind} snapshot {name}"));
    }
    if is_empty(&loaded.book) {
        warn(err, empty_note);
    }
    Ok(loaded.book)
}

/// Writes a warning line. Failing to write one is not worth stopping for.
fn warn(err: &mut impl Write, message: impl Display) {
    let _ = writeln!(err, "{message}");
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::refresh::apply;
    use crate::report::Colors;
    use crate::snapshots::last_checked;
    use crate::testing::{SAMPLE_PAGE, SAMPLE_TABLE, ScratchDir};

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
    fn mode_pairs_with_download_only_when_online() {
        let download = || -> Result<Downloads, Box<dyn Error>> { Err("unused".into()) };
        assert!(matches!(
            Mode::Offline.with_download(download),
            SourceRefresh::Offline
        ));
        assert!(matches!(
            Mode::Online.with_download(download),
            SourceRefresh::Online(_)
        ));
    }

    #[test]
    fn offline_run_with_nothing_saved_warns_and_still_reports() {
        let home = ScratchDir::new("app-empty");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            &home,
            SourceRefresh::offline(),
            at(),
            Colors::Off,
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
        let dirs = SnapshotDirs::under(&home);
        apply(
            &dirs.prices,
            &dirs.multipliers,
            at(),
            SAMPLE_TABLE,
            SAMPLE_PAGE,
        )
        .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            &home,
            SourceRefresh::offline(),
            at(),
            Colors::Off,
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
    fn pi_sessions_appear_in_the_priced_report() {
        let home = ScratchDir::new("app-pi");
        let logs = home.join(".pi/agent/sessions/--work-pi-app--");
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("session.jsonl"),
            concat!(
                r#"{"type":"session","version":3,"timestamp":"2026-10-05T15:04:35Z","cwd":"/work/pi-app"}"#,
                "\n",
                r#"{"type":"message","message":{"role":"assistant","model":"gpt-5.6-sol","usage":{"input":1000000,"output":1000000,"cost":{"total":0.0}}}}"#,
            ),
        )
        .unwrap();
        let dirs = SnapshotDirs::under(&home);
        apply(
            &dirs.prices,
            &dirs.multipliers,
            at(),
            SAMPLE_TABLE,
            SAMPLE_PAGE,
        )
        .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            &home,
            SourceRefresh::offline(),
            at(),
            Colors::Off,
            &mut out,
            &mut err,
        )
        .unwrap();
        let report = String::from_utf8(out).unwrap();
        assert!(report.contains("pi"), "{report}");
        assert!(report.contains("pi-app"), "{report}");
        assert!(report.contains("2000000"), "{report}");
        assert!(report.contains("10.00"), "{report}");
        assert!(!String::from_utf8(err).unwrap().contains("pi:"));
    }

    #[test]
    fn a_corrupt_snapshot_is_named_in_a_warning() {
        let home = ScratchDir::new("app-corrupt");
        let dirs = SnapshotDirs::under(&home);
        fs::create_dir_all(&dirs.prices).unwrap();
        fs::write(dirs.prices.join("20261001T000000Z.json"), "{truncated").unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            &home,
            SourceRefresh::offline(),
            at(),
            Colors::Off,
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
            SourceRefresh::Online(download),
            at(),
            Colors::Off,
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
            SourceRefresh::Online(download),
            at(),
            Colors::Off,
            &mut out,
            &mut err,
        )
        .unwrap();

        let err = String::from_utf8(err).unwrap();
        assert!(!err.contains("no price snapshot saved"), "{err}");
        assert!(!err.contains("no multiplier snapshot saved"), "{err}");
        assert!(
            SnapshotDirs::under(&home).prices.is_dir(),
            "a successful download creates the prices directory before prices are read"
        );
    }

    #[test]
    fn color_reaches_the_report() {
        let home = ScratchDir::new("app-color");
        let (mut colored, mut plain, mut err) = (Vec::new(), Vec::new(), Vec::new());
        run(
            &home,
            SourceRefresh::offline(),
            at(),
            Colors::On,
            &mut colored,
            &mut err,
        )
        .unwrap();
        run(
            &home,
            SourceRefresh::offline(),
            at(),
            Colors::Off,
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
                SourceRefresh::offline(),
                at(),
                Colors::Off,
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
                SourceRefresh::offline(),
                at(),
                Colors::Off,
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
        let dirs = SnapshotDirs::under(&home);
        apply(
            &dirs.prices,
            &dirs.multipliers,
            at(),
            SAMPLE_TABLE,
            SAMPLE_PAGE,
        )
        .unwrap();

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let later = at() + chrono::TimeDelta::days(3);
        run(
            &home,
            SourceRefresh::offline(),
            later,
            Colors::Off,
            &mut out,
            &mut err,
        )
        .unwrap();

        assert_eq!(
            last_checked(&dirs.prices),
            Some(at()),
            "an offline run does not change the recorded price check time"
        );
    }
}
