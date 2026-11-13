//! Runs the built binary against a scratch home folder, so the entry point is tested too.

pub mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::ScratchDir;
use snapbox::{Assert, Data};

fn run_binary(args: &[&str], home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_droid-usage"))
        .args(args)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

#[test]
fn offline_run_prints_the_report_and_warns_about_missing_snapshots() {
    let home = ScratchDir::new("binary-offline");
    let output = run_binary(&["--offline"], &home);

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("By source"), "{stdout}");
    assert!(stderr.contains("no price snapshot saved"), "{stderr}");
}

#[test]
fn missing_home_variable_is_named_in_the_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_droid-usage"))
        .arg("--offline")
        .env_remove("HOME")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("HOME"),
        "the error names the HOME variable: {stderr}"
    );
}

#[test]
fn unknown_argument_fails_and_is_named() {
    let home = ScratchDir::new("binary-bad-arg");
    let output = run_binary(&["--bogus"], &home);

    assert!(
        !output.status.success(),
        "an unknown flag makes the run exit with a failure status"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--bogus"), "{stderr}");
}

// Drift check: the regeneration command in `tests/golden/anonymize.py` (the binary, with HOME set to the fixtures
// and `NO_COLOR=1`) must reproduce the saved golden files byte for byte.
#[test]
fn golden_fixtures_reproduce_the_saved_output_through_the_binary() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let output = run_binary(&["--offline"], &golden.join("home"));

    assert!(output.status.success(), "{output:?}");
    let want_report = Data::read_from(&golden.join("report.txt"), None).raw();
    let want_warnings = Data::read_from(&golden.join("warnings.txt"), None).raw();
    Assert::new().eq(String::from_utf8(output.stdout).unwrap(), want_report);
    Assert::new().eq(String::from_utf8(output.stderr).unwrap(), want_warnings);
}
