//! Reader for Pi sessions under `~/.pi/agent/sessions`.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

use crate::model::{Read, Session, Source, Tokens, files_with_suffix, parse_timestamp};

/// Reads every Pi session under the given sessions directory.
/// A missing session directory is normal when Pi has not been used.
///
/// # Errors
///
/// Returns an error when the session directory cannot be searched, except when it is missing.
pub fn read_sessions(root: &Path) -> io::Result<Read> {
    let found = match files_with_suffix(root, ".jsonl") {
        Ok(found) => found,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Read::default()),
        Err(error) => return Err(error),
    };
    let mut read = Read {
        skipped: found.unreadable,
        ..Read::default()
    };
    for path in found
        .files
        .into_iter()
        // Pi stores sessions at `<root>/<project>/*.jsonl`; deeper folders hold subagent artifacts and child runs already counted in parent tool results.
        .filter(|path| path.parent().and_then(Path::parent) == Some(root))
    {
        match File::open(path)
            .ok()
            .and_then(|file| parse_transcript(BufReader::new(file)))
        {
            Some(session) => read.sessions.push(session),
            None => read.skipped += 1,
        }
    }
    Ok(read)
}

/// Reads metadata, summed token usage, and recorded cost from a Pi session transcript.
/// Invalid JSON lines are skipped; an I/O error or missing session timestamp rejects the file.
#[must_use]
pub fn parse_transcript(reader: impl BufRead) -> Option<Session> {
    let (mut started, mut project, mut model, mut cost_usd) = (None, None, None, None);
    let mut tokens = Tokens::default();
    for line in reader.lines() {
        let line = line.ok()?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match value["type"].as_str() {
            Some("session") if started.is_none() => {
                started = value["timestamp"].as_str().and_then(parse_timestamp);
                project = value["cwd"].as_str().map(str::to_owned);
            }
            Some("model_change") => {
                // shortcut: model switches use the latest model, split usage by model if needed.
                if let Some(name) = value["modelId"].as_str() {
                    model = Some(name.to_owned());
                }
            }
            Some("message") => {
                let message = &value["message"];
                if let Some(name) = message["model"].as_str() {
                    model = Some(name.to_owned());
                }
                add_usage(&message["usage"], &mut tokens, &mut cost_usd);
            }
            Some("usage" | "compaction" | "branch_summary") => {
                if let Some(name) = value["model"].as_str() {
                    model = Some(name.to_owned());
                }
                add_usage(&value["usage"], &mut tokens, &mut cost_usd);
            }
            _ => {}
        }
    }
    Some(Session {
        source: Source::Pi,
        started: started?,
        project,
        model,
        tokens,
        cost_usd,
    })
}

fn add_usage(usage: &Value, tokens: &mut Tokens, cost_usd: &mut Option<f64>) {
    let (Some(input), Some(output)) = (usage["input"].as_u64(), usage["output"].as_u64()) else {
        return;
    };
    // Pi reports uncached input in `input` and includes reasoning in `output`.
    tokens.add(&Tokens {
        input,
        output,
        cache_read: usage["cacheRead"].as_u64().unwrap_or(0),
        cache_write: usage["cacheWrite"].as_u64().unwrap_or(0),
    });
    // Custom providers without configured rates record 0; leave those None for price-table lookup.
    if let Some(cost) = usage["cost"]["total"].as_f64().filter(|cost| *cost > 0.0) {
        *cost_usd = Some(cost_usd.unwrap_or(0.0) + cost);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

    use proptest::prelude::proptest;

    use super::*;
    use crate::testing::ScratchDir;

    const HEADER: &str = concat!(
        r#"{"type":"session","version":3,"timestamp":"2026-10-05T15:04:35Z","cwd":"/work/app"}"#,
        "\n",
        r#"{"type":"model_change","modelId":"gemini-3.6-flash"}"#,
        "\n",
    );

    #[test]
    fn sums_usage_and_positive_costs_across_entry_kinds() {
        let text = concat!(
            r#"{"type":"session","version":3,"timestamp":"2026-10-05T15:04:35Z","cwd":"/work/app"}"#,
            "\n",
            r#"{"type":"model_change","modelId":"gemini-3.1-pro"}"#,
            "\n",
            r#"{"type":"message","message":{"role":"assistant","model":"gemini-3.6-flash","usage":{"input":10,"output":4,"cacheRead":2,"cacheWrite":1,"reasoning":3,"cost":{"total":0.25}}}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"toolResult","usage":{"input":3,"output":2,"cacheRead":1,"cacheWrite":0,"cost":{"total":0.5}}}}"#,
            "\n",
            r#"{"type":"usage","kind":"cache_warm","model":"claude-sonnet-4-5","usage":{"input":0,"output":0,"cacheRead":5,"cacheWrite":2,"cost":{"total":0.125}}}"#,
            "\n",
            r#"{"type":"compaction","usage":{"input":4,"output":1,"cost":{"total":0.0}}}"#,
            "\n",
            r#"{"type":"branch_summary","usage":{"input":2,"output":3}}"#,
        );
        let session = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(session.source.to_string(), "pi");
        assert_eq!(
            session.started,
            parse_timestamp("2026-10-05T15:04:35Z").unwrap()
        );
        assert_eq!(session.project.as_deref(), Some("/work/app"));
        assert_eq!(session.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(
            session.tokens,
            Tokens {
                input: 19,
                output: 10,
                cache_read: 8,
                cache_write: 3,
            }
        );
        assert_eq!(session.cost_usd, Some(0.875));
    }

    #[test]
    fn zero_or_negative_costs_leave_session_unpriced_for_table_lookup() {
        let text = format!(
            "{HEADER}{}\n{}",
            r#"{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":4,"cost":{"total":0.0}}}}"#,
            r#"{"type":"message","message":{"role":"assistant","usage":{"input":5,"output":2,"cost":{"total":-1.0}}}}"#,
        );
        let session = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(session.tokens.total(), 21);
        assert_eq!(session.cost_usd, None);
    }

    #[test]
    fn malformed_and_incomplete_entries_do_not_erase_metadata_or_usage() {
        let text = format!(
            "{HEADER}{}\ngarbage\n{}\n{}\n{}\n{}\n{}",
            r#"{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":4}}}"#,
            r#"{"type":"session","timestamp":"2026-11-01T00:00:00Z","cwd":"/other"}"#,
            r#"{"type":"model_change"}"#,
            r#"{"type":"message","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"usage","usage":{"input":5}}"#,
            r#"{"type":"custom","customType":"note"}"#,
        );
        let session = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(
            session.started,
            parse_timestamp("2026-10-05T15:04:35Z").unwrap()
        );
        assert_eq!(session.project.as_deref(), Some("/work/app"));
        assert_eq!(session.model.as_deref(), Some("gemini-3.6-flash"));
        assert_eq!(session.tokens.total(), 14);
    }

    #[test]
    fn metadata_only_and_missing_optional_fields_are_supported() {
        let session = parse_transcript(Cursor::new(
            r#"{"type":"session","timestamp":"2026-10-05T15:04:35Z"}"#,
        ))
        .unwrap();
        assert_eq!(session.tokens, Tokens::default());
        assert_eq!(session.model, None);
        assert_eq!(session.project, None);
        assert_eq!(session.cost_usd, None);
        assert!(parse_transcript(Cursor::new("garbage\n{}")).is_none());
    }

    #[test]
    fn read_errors_reject_partial_sessions() {
        let mut text = HEADER.as_bytes().to_vec();
        text.extend_from_slice(&[0xff, b'\n']);
        assert!(parse_transcript(Cursor::new(text)).is_none());
    }

    #[test]
    fn reads_nested_logs_and_counts_unreadable_files() {
        let root = ScratchDir::new("pi-reader");
        let project = root.join("--work-app--");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("a.jsonl"),
            format!(
                "{HEADER}{}",
                r#"{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":4,"cacheRead":2}}}"#
            ),
        )
        .unwrap();
        fs::write(project.join("b.jsonl"), HEADER).unwrap();
        fs::write(project.join("bad.jsonl"), "garbage").unwrap();
        fs::write(project.join("ignore.txt"), "garbage").unwrap();
        fs::write(root.join("stray.jsonl"), HEADER).unwrap();
        let artifacts = project.join("subagent-artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("child_transcript.jsonl"), "garbage").unwrap();
        let read = read_sessions(&root).unwrap();
        assert_eq!(read.sessions.len(), 2);
        assert_eq!(read.sessions[0].tokens.total(), 16);
        assert_eq!(read.sessions[1].tokens.total(), 0);
        assert_eq!(read.skipped, 1);
        assert_eq!(read.models_unreadable, 0);
        assert_eq!(
            read_sessions(&root.join("missing")).unwrap().sessions,
            Vec::new(),
            "missing Pi logs are an empty source"
        );
    }

    #[test]
    fn a_session_directory_that_is_a_file_is_an_error() {
        let root = ScratchDir::new("pi-not-directory");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("sessions");
        fs::write(&file, "").unwrap();
        assert!(read_sessions(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folders_and_files_are_counted_without_losing_sessions() {
        use std::os::unix::fs::PermissionsExt;

        let root = ScratchDir::new("pi-unreadable");
        let project = root.join("--work-app--");
        let locked = project.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(project.join("good.jsonl"), HEADER).unwrap();
        let looping = project.join("loop.jsonl");
        std::os::unix::fs::symlink(&looping, &looping).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let read = read_sessions(&root);

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let read = read.unwrap();
        assert_eq!(read.sessions.len(), 1, "the readable session is kept");
        assert_eq!(read.skipped, 2, "both unreadable entries are counted");
    }

    proptest! {
        #[test]
        fn parser_never_panics(text in ".*") {
            let _ = parse_transcript(Cursor::new(text));
        }
    }
}
