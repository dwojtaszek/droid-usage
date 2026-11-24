//! Command-line arguments. The parser lives in the library so its tests can reach it.

use std::error::Error;
use std::fmt::{self, Display};

use chrono::NaiveDate;

use crate::app::Mode;

/// Parsed command-line arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Args {
    /// Whether prices are refreshed before the report.
    pub mode: Mode,
    /// Only sessions that started on or after this UTC date are counted.
    pub since: Option<NaiveDate>,
}

/// An argument other than `--offline` or `--since`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownArgument(String);

impl Display for UnknownArgument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown argument {}; the options are --offline and --since YYYY-MM-DD",
            self.0
        )
    }
}

impl Error for UnknownArgument {}

/// A `--since` value that is not a `YYYY-MM-DD` date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSince(String);

impl Display for InvalidSince {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid --since value {}; expected YYYY-MM-DD", self.0)
    }
}

impl Error for InvalidSince {}

/// A `--since` flag with no date after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingSince;

impl Display for MissingSince {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("--since needs a YYYY-MM-DD date")
    }
}

impl Error for MissingSince {}

/// Why the command-line arguments could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// An argument other than `--offline` or `--since`.
    Unknown(UnknownArgument),
    /// A `--since` value that is not a `YYYY-MM-DD` date.
    InvalidSince(InvalidSince),
    /// A `--since` flag with no date after it.
    MissingSince(MissingSince),
}

impl Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(error) => error.fmt(f),
            Self::InvalidSince(error) => error.fmt(f),
            Self::MissingSince(error) => error.fmt(f),
        }
    }
}

impl Error for ArgError {}

/// Parses a `--since` date as `YYYY-MM-DD`. The time of day is UTC midnight.
///
/// # Errors
///
/// Returns an [`InvalidSince`] that names the bad value.
pub fn parse_since(text: &str) -> Result<NaiveDate, InvalidSince> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_error| InvalidSince(text.to_owned()))
}

/// Reads the arguments: `--offline` selects [`Mode::Offline`], and `--since YYYY-MM-DD` keeps only sessions started on or after that date. No arguments select online mode with every session.
///
/// # Errors
///
/// Returns an [`ArgError`] that names the first offending argument or value.
pub fn parse_args<I, S>(args: I) -> Result<Args, ArgError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut mode = Mode::Online;
    let mut since = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--offline" => mode = Mode::Offline,
            "--since" => {
                let value = args.next().ok_or(ArgError::MissingSince(MissingSince))?;
                let date = parse_since(value.as_ref()).map_err(ArgError::InvalidSince)?;
                since = Some(date);
            }
            other => {
                return Err(ArgError::Unknown(UnknownArgument(other.to_owned())));
            }
        }
    }
    Ok(Args { mode, since })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_means_online_without_a_since_date() {
        assert_eq!(
            parse_args(Vec::<&str>::new()),
            Ok(Args {
                mode: Mode::Online,
                since: None,
            }),
            "no arguments select online mode and every session"
        );
    }

    #[test]
    fn offline_flag_is_read() {
        assert_eq!(
            parse_args(["--offline"]),
            Ok(Args {
                mode: Mode::Offline,
                since: None,
            }),
            "the offline flag selects offline mode"
        );
    }

    #[test]
    fn since_flag_is_read_as_a_date() {
        assert_eq!(
            parse_args(["--offline", "--since", "2026-10-24"]),
            Ok(Args {
                mode: Mode::Offline,
                since: Some(NaiveDate::from_ymd_opt(2026, 10, 24).unwrap()),
            }),
            "the since flag keeps sessions from its date on"
        );
    }

    #[test]
    fn an_invalid_since_date_is_named() {
        let err = parse_args(["--since", "2026-13-40"]).unwrap_err();
        assert!(
            err.to_string().contains("2026-13-40"),
            "the error names the bad value: {err}"
        );
    }

    #[test]
    fn a_missing_since_date_is_an_error() {
        let err = parse_args(["--since"]).unwrap_err();
        assert!(err.to_string().contains("--since"), "{err}");
    }

    #[test]
    fn unknown_argument_is_named() {
        let err = parse_args(["--offline", "--bogus"]).unwrap_err();
        assert!(err.to_string().contains("--bogus"), "{err}");
    }
}
