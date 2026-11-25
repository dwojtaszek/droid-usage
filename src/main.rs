//! Command-line entry point. It supplies the real home folder, clock, terminal, and network to the library.

use std::env;
use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use droid_usage::{app, cli, download, multipliers, prices};
use jiff::Timestamp;

/// How long each download may take before it is abandoned.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

#[expect(
    clippy::disallowed_methods,
    reason = "CLI entry point reads the host environment and wall clock"
)]
fn main() -> Result<(), Box<dyn Error>> {
    let args = cli::parse_args(env::args().skip(1))?;
    let mode = args.mode;
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
    let stdout = if console::colors_enabled() {
        app::Stdout::Terminal
    } else {
        app::Stdout::Redirected
    };
    let no_color = if env::var_os("NO_COLOR").is_some() {
        app::NoColor::Set
    } else {
        app::NoColor::Unset
    };
    let refresh = mode.with_download(|| {
        download::fetch_both(
            prices::SOURCE_URL,
            multipliers::SOURCE_URL,
            DOWNLOAD_TIMEOUT,
        )
        .map_err(Into::into)
    });
    app::run(
        &home,
        refresh,
        args.since,
        Timestamp::now(),
        app::use_color(stdout, no_color),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}
