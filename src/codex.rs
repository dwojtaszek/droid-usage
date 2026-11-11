//! Reader for Codex CLI rollout logs under `~/.codex`.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

use crate::model::{Read, Session, Source, Tokens, files_with_suffix, parse_timestamp};

/// Reads active and archived Codex sessions under the given Codex home directory.
/// Missing session directories are normal when Codex has not been used.
///
/// # Errors
///
/// Returns an error when a session directory cannot be searched, except when it is missing.
pub fn read_sessions(root: &Path) -> io::Result<Read> {
    let mut read = Read::default();
    for directory in ["sessions", "archived_sessions"] {
        let found = match files_with_suffix(&root.join(directory), ".jsonl") {
            Ok(found) => found,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        read.skipped += found.unreadable;
        for path in found.files {
            match File::open(path)
                .ok()
                .and_then(|file| parse_transcript(BufReader::new(file)))
            {
                Some(session) => read.sessions.push(session),
                None => read.skipped += 1,
            }
        }
    }
    Ok(read)
}

/// Reads metadata and the latest cumulative usage from a Codex rollout.
/// Invalid JSON lines are skipped; an I/O error or missing session timestamp rejects the file.
#[must_use]
pub fn parse_transcript(reader: impl BufRead) -> Option<Session> {
    let (mut started, mut project, mut model) = (None, None, None);
    let mut tokens = Tokens::default();
    for line in reader.lines() {
        let line = line.ok()?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let payload = &value["payload"];
        match value["type"].as_str() {
            Some("session_meta") if started.is_none() => {
                started = payload["timestamp"].as_str().and_then(parse_timestamp);
                project = payload["cwd"].as_str().map(str::to_owned);
            }
            Some("turn_context") => {
                // shortcut: model switches use the latest context, split usage by model if needed.
                if let Some(name) = payload["model"].as_str() {
                    model = Some(name.to_owned());
                }
            }
            Some("event_msg") if payload["type"] == "token_count" => {
                if let Some(usage) = parse_usage(&payload["info"]["total_token_usage"]) {
                    tokens = usage;
                }
            }
            Some("token_usage_record") => {
                if let Some(usage) = parse_usage(&payload["thread_token_usage"]) {
                    tokens = usage;
                }
            }
            _ => {}
        }
    }
    Some(Session {
        source: Source::Codex,
        started: started?,
        project,
        model,
        tokens,
        cost_usd: None,
    })
}

fn parse_usage(usage: &Value) -> Option<Tokens> {
    let input = usage["input_tokens"].as_u64()?;
    let output = usage["output_tokens"].as_u64()?;
    let cache_read = usage["cached_input_tokens"].as_u64().unwrap_or(0);
    let cache_write = usage["cache_write_input_tokens"].as_u64().unwrap_or(0);
    // Codex includes cached input in input_tokens and reasoning in output_tokens.
    Some(Tokens {
        input: input.saturating_sub(cache_read).saturating_sub(cache_write),
        output,
        cache_read,
        cache_write,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

    use proptest::prelude::proptest;

    use super::*;
    use crate::testing::ScratchDir;

    const HEADER: &str = concat!(
        r#"{"type":"session_meta","payload":{"timestamp":"2026-10-05T15:04:35Z","cwd":"/work/app"}}"#,
        "\n",
        r#"{"type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
        "\n",
    );

    fn event(input: u64, cached: u64, output: u64) -> String {
        format!(
            r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"reasoning_output_tokens":3}},"last_token_usage":{{"input_tokens":999}}}}}}}}"#
        )
    }

    #[test]
    fn cumulative_counts_are_replaced_and_cache_and_reasoning_are_not_doubled() {
        let text = format!(
            "{HEADER}{}\n{}\n{}",
            event(10, 2, 4),
            event(30, 8, 9),
            event(30, 8, 9)
        );
        let session = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(session.source.to_string(), "codex");
        assert_eq!(
            session.started,
            parse_timestamp("2026-10-05T15:04:35Z").unwrap()
        );
        assert_eq!(session.project.as_deref(), Some("/work/app"));
        assert_eq!(session.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(
            session.tokens,
            Tokens {
                input: 22,
                output: 9,
                cache_read: 8,
                cache_write: 0
            }
        );
        assert_eq!(session.tokens.total(), 39);
        assert_eq!(session.cost_usd, None);
    }

    #[test]
    fn usage_records_and_events_share_one_cumulative_total() {
        let text = format!(
            "{HEADER}{}\n{}",
            event(30, 8, 9),
            r#"{"type":"token_usage_record","payload":{"usage":{"input_tokens":999},"thread_token_usage":{"input_tokens":40,"cached_input_tokens":10,"cache_write_input_tokens":2,"output_tokens":12}}}"#
        );
        let session = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(
            session.tokens,
            Tokens {
                input: 28,
                output: 12,
                cache_read: 10,
                cache_write: 2
            }
        );
    }

    #[test]
    fn malformed_and_null_events_do_not_erase_usage_or_metadata() {
        let text = format!(
            "{HEADER}{}\ngarbage\n{}\n{}\n{}\n{}\n{}",
            event(10, 2, 4),
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#,
            r#"{"type":"event_msg","payload":{"type":"other","info":{"total_token_usage":{"input_tokens":99,"output_tokens":99}}}}"#,
            r#"{"type":"turn_context","payload":{}}"#,
            r#"{"type":"session_meta","payload":{"timestamp":"2026-11-01T00:00:00Z","cwd":"/other"}}"#,
            r#"{"type":"token_usage_record","payload":{"thread_token_usage":{"input_tokens":-1,"output_tokens":8}}}"#
        );
        let session = parse_transcript(Cursor::new(text)).unwrap();
        assert_eq!(session.tokens.total(), 14);
        assert_eq!(session.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(session.project.as_deref(), Some("/work/app"));
        assert_eq!(
            session.started,
            parse_timestamp("2026-10-05T15:04:35Z").unwrap()
        );
    }

    #[test]
    fn metadata_only_and_missing_optional_fields_are_supported() {
        let session = parse_transcript(Cursor::new(
            r#"{"type":"session_meta","payload":{"timestamp":"2026-10-05T15:04:35Z"}}"#,
        ))
        .unwrap();
        assert_eq!(session.tokens, Tokens::default());
        assert_eq!(session.model, None);
        assert_eq!(session.project, None);
        assert!(parse_transcript(Cursor::new("garbage\n{}")).is_none());
    }

    #[test]
    fn usage_defaults_optional_counts_and_saturates_invalid_cache_counts() {
        assert_eq!(
            parse_usage(&serde_json::json!({"input_tokens":5,"output_tokens":2})),
            Some(Tokens {
                input: 5,
                output: 2,
                ..Tokens::default()
            })
        );
        assert_eq!(
            parse_usage(
                &serde_json::json!({"input_tokens":1,"cached_input_tokens":9,"output_tokens":2})
            )
            .unwrap()
            .input,
            0
        );
        assert!(parse_usage(&serde_json::json!({"input_tokens":1})).is_none());
    }

    #[test]
    fn latest_model_context_is_kept() {
        let text = format!(
            "{HEADER}{}",
            r#"{"type":"turn_context","payload":{"model":"other"}}"#
        );
        assert_eq!(
            parse_transcript(Cursor::new(text))
                .unwrap()
                .model
                .as_deref(),
            Some("other")
        );
    }

    #[test]
    fn read_errors_reject_partial_sessions() {
        let mut text = HEADER.as_bytes().to_vec();
        text.extend_from_slice(&[0xff, b'\n']);
        assert!(parse_transcript(Cursor::new(text)).is_none());
    }

    #[test]
    fn reads_nested_and_archived_logs_and_counts_unreadable_files() {
        let root = ScratchDir::new("codex-reader");
        fs::create_dir_all(root.join("sessions/2026/10/05")).unwrap();
        fs::create_dir_all(root.join("archived_sessions")).unwrap();
        fs::write(
            root.join("sessions/2026/10/05/a.jsonl"),
            format!("{HEADER}{}", event(10, 2, 4)),
        )
        .unwrap();
        fs::write(root.join("archived_sessions/b.jsonl"), HEADER).unwrap();
        fs::write(root.join("sessions/bad.jsonl"), "garbage").unwrap();
        fs::write(root.join("sessions/ignore.txt"), "garbage").unwrap();
        let read = read_sessions(&root).unwrap();
        assert_eq!(read.sessions.len(), 2);
        assert_eq!(read.sessions[0].tokens.total(), 14);
        assert_eq!(read.sessions[1].tokens.total(), 0);
        assert_eq!(read.skipped, 1);
        assert_eq!(read.models_unreadable, 0);
        assert_eq!(
            read_sessions(&root.join("missing")).unwrap().sessions,
            Vec::new(),
            "missing Codex logs are an empty source"
        );
    }

    #[test]
    fn a_session_directory_that_is_a_file_is_an_error() {
        let root = ScratchDir::new("codex-not-directory");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sessions"), "").unwrap();
        assert!(read_sessions(&root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folders_and_files_are_counted_without_losing_sessions() {
        use std::os::unix::fs::PermissionsExt;

        let root = ScratchDir::new("codex-unreadable");
        let logs = root.join("sessions");
        let locked = logs.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(logs.join("good.jsonl"), HEADER).unwrap();
        let looping = logs.join("loop.jsonl");
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
