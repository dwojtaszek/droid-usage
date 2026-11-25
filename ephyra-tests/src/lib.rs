//! Isolated Ephyra property-based testing harness for `droid-usage`.

pub mod cli_droid;
pub mod multipliers;
pub mod opencode_agy;
pub mod prices;
pub mod pricing_report;
pub mod transcripts;

/// Default Ephyra configuration pinning coverage attribution to `droid_usage`, even when run from the repository root via `--manifest-path`.
#[must_use]
pub fn droid_usage_config() -> ephyra::Config {
    let mut cfg = ephyra::Config::default();
    "droid_usage".clone_into(&mut cfg.coverage.filter);
    cfg
}
