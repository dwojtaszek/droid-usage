//! Reader for Droid CLI sessions under `~/.factory/sessions`.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::ops::ControlFlow;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::files::{LineError, files_with_suffix, for_each_json_line};
use crate::model::{Read, Session, Source, Tokens, parse_timestamp};

/// Reads every Droid session under `root`. Each session is a `<id>.settings.json` (usage) next to `<id>.jsonl` (project and start time).
///
/// # Errors
///
/// Returns an error when `root` cannot be read. Folders under it that cannot be read are skipped and counted.
pub fn read_sessions(root: &Path) -> std::io::Result<Read> {
    let found = files_with_suffix(root, ".settings.json")?;
    let mut read = Read {
        skipped: found.unreadable,
        ..Read::default()
    };
    for settings in found.files {
        match session_from_files(&settings) {
            Some(session) => read.sessions.push(session),
            None => read.skipped += 1,
        }
    }
    Ok(read)
}

fn session_from_files(settings_path: &Path) -> Option<Session> {
    let name = settings_path.file_name()?.to_str()?;
    let id = name.strip_suffix(".settings.json")?;
    let transcript_path = settings_path.with_file_name(format!("{id}.jsonl"));

    let (tokens, model) = parse_settings(&std::fs::read_to_string(settings_path).ok()?)?;
    let file = File::open(transcript_path).ok()?;
    let (project, started) = parse_header(BufReader::new(file))?;

    Some(Session {
        source: Source::Droid,
        started,
        project,
        model,
        tokens,
        cost_usd: None,
    })
}

/// Usage and model from a `.settings.json` document. The session's own usage only, not its children's.
pub fn parse_settings(text: &str) -> Option<(Tokens, Option<String>)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let usage = &value["tokenUsage"];
    let tokens = Tokens {
        input: usage["inputTokens"].as_u64().unwrap_or(0),
        output: usage["outputTokens"].as_u64().unwrap_or(0),
        cache_read: usage["cacheReadTokens"].as_u64().unwrap_or(0),
        cache_write: usage["cacheCreationTokens"].as_u64().unwrap_or(0),
    };
    let model = value["model"].as_str().map(str::to_owned);
    Some((tokens, model))
}

/// Project directory and start time from a session transcript. Stops reading once both are known.
pub fn parse_header(reader: impl BufRead) -> Option<(Option<String>, DateTime<Utc>)> {
    let mut project = None;
    let mut started = None;
    // A line that is not valid UTF-8 or JSON is skipped, not fatal.
    let _ = for_each_json_line(reader, LineError::Skip, |value| {
        if value["type"] == "session_start" && project.is_none() {
            project = value["cwd"].as_str().map(str::to_owned);
        }
        if value["type"] == "message"
            && let Some(time) = value["timestamp"].as_str().and_then(parse_timestamp)
        {
            started = Some(time);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    started.map(|started| (project, started))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use proptest::prelude::proptest;

    use super::*;
    use crate::testing::ScratchDir;

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_counted_as_skipped() {
        use std::os::unix::fs::PermissionsExt;

        let root = ScratchDir::new("droid-locked-folder");
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let read = read_sessions(&root);

        // Restore access so the scratch folder can be removed, whatever the result.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let read = read.unwrap();
        assert_eq!(read.skipped, 1, "the locked folder is counted as skipped");
        assert!(
            read.sessions.is_empty(),
            "no session comes from a locked folder"
        );
    }

    #[test]
    fn settings_give_own_usage_and_model() {
        let text = r#"{"model":"glm-5.3","tokenUsage":{"inputTokens":10,"outputTokens":4,"cacheReadTokens":2,"cacheCreationTokens":1},"inclusiveTokenUsage":{"inputTokens":999}}"#;
        let (tokens, model) = parse_settings(text).unwrap();
        assert_eq!(
            tokens,
            Tokens {
                input: 10,
                output: 4,
                cache_read: 2,
                cache_write: 1
            },
            "settings usage comes from the session's own token fields"
        );
        assert_eq!(
            model.as_deref(),
            Some("glm-5.3"),
            "the model name comes from the settings model field"
        );
    }

    #[test]
    fn settings_without_usage_count_as_zero() {
        let (tokens, model) = parse_settings(r#"{"model":"x"}"#).unwrap();
        assert_eq!(
            tokens,
            Tokens::default(),
            "settings without usage give zero tokens"
        );
        assert_eq!(
            model.as_deref(),
            Some("x"),
            "settings without usage still report the model"
        );
    }

    #[test]
    fn settings_reject_invalid_json() {
        assert!(
            parse_settings("{not json").is_none(),
            "invalid settings json gives no result"
        );
    }

    #[test]
    fn header_takes_project_and_first_message_time() {
        let text = concat!(
            r#"{"type":"session_start","cwd":"/work/app"}"#,
            "\n",
            r#"{"type":"message","timestamp":"2026-10-05T15:04:35Z","message":{}}"#,
            "\n",
            r#"{"type":"message","timestamp":"2026-10-06T00:00:00Z","message":{}}"#,
            "\n",
        );
        let (project, started) = parse_header(Cursor::new(text)).unwrap();
        assert_eq!(
            project.as_deref(),
            Some("/work/app"),
            "the project comes from the session start cwd"
        );
        assert_eq!(
            started.to_rfc3339(),
            "2026-10-05T15:04:35+00:00",
            "the start time is the first message timestamp in utc"
        );
    }

    #[test]
    fn header_needs_a_message_timestamp_and_skips_bad_lines() {
        let no_timestamp = "garbage\n{\"type\":\"session_start\",\"cwd\":\"/p\"}\n";
        assert!(
            parse_header(Cursor::new(no_timestamp)).is_none(),
            "a header without a message timestamp gives no result"
        );

        let no_project = "garbage\n{\"type\":\"message\",\"timestamp\":\"2026-10-05T15:04:35Z\"}\n";
        let (project, _) = parse_header(Cursor::new(no_project)).unwrap();
        assert_eq!(project, None, "a header without a cwd gives no project");
    }

    #[test]
    fn droid_header_keeps_the_first_project_seen() {
        let text = concat!(
            r#"{"type":"session_start","cwd":"/first"}"#,
            "\n",
            r#"{"type":"session_start","cwd":"/second"}"#,
            "\n",
            r#"{"type":"message","timestamp":"2026-10-05T15:04:35Z"}"#,
            "\n",
        );
        let (project, _) = parse_header(Cursor::new(text)).unwrap();
        assert_eq!(
            project.as_deref(),
            Some("/first"),
            "the first project seen is kept over later ones"
        );
    }

    proptest! {
        #[test]
        fn parsers_never_panic_on_arbitrary_text(text in ".*") {
            let _ = parse_settings(&text);
            let _ = parse_header(Cursor::new(text));
        }
    }
}
