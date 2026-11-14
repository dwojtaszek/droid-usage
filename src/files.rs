//! Local directory walking and JSONL transcript scanning shared by session readers.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Files found under a folder, and how many folders or entries under it could not be read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Matching files, sorted.
    pub files: Vec<PathBuf>,
    /// Folders and entries skipped because they could not be read.
    pub unreadable: usize,
}

/// How [`for_each_json_line`] reacts when reading a line fails (I/O error or invalid UTF-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineError {
    /// Stop reading and report the file as unreadable (`Ok(false)`).
    Abort,
    /// Skip the unreadable line and continue with the next line.
    Skip,
}

/// Every file under `root` whose name ends with `suffix`, searched recursively. A folder or entry under `root` that cannot be read is skipped and counted.
///
/// # Errors
///
/// Returns an error, naming `root`, when `root` itself cannot be read.
pub fn files_with_suffix(root: &Path, suffix: &str) -> io::Result<Found> {
    walk_suffix(root, suffix, None)
}

/// Every file under `root` whose name ends with `suffix` at `depth` subdirectories below `root` (`depth = 1` means `<root>/<dir>/*<suffix>`), without descending deeper than `depth + 1`.
///
/// # Errors
///
/// Returns an error, naming `root`, when `root` itself cannot be read.
pub fn files_at_depth(root: &Path, suffix: &str, depth: usize) -> io::Result<Found> {
    walk_suffix(root, suffix, Some(depth))
}

fn open_root(root: &Path) -> io::Result<fs::ReadDir> {
    fs::read_dir(root)
        .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", root.display())))
}

fn walk_suffix(root: &Path, suffix: &str, target_depth: Option<usize>) -> io::Result<Found> {
    // Opening the root first makes a missing or unreadable root an error. Folders below it are skipped.
    drop(open_root(root)?);
    let mut found = Found::default();
    let mut seen = BTreeSet::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = pending.pop() {
        // A folder reached through a symlink, or a link back up the tree, is read once.
        // If the folder cannot be canonicalized, it is still checked as given, and the read below counts it.
        let canonical = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !seen.insert(canonical) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            found.unreadable += 1;
            continue;
        };
        let descend = target_depth.is_none_or(|limit| depth <= limit);
        let collect_files = target_depth.is_none_or(|target| depth == target);
        let mut children = Vec::new();
        sort_entries(
            entries.map(|entry| entry.map(|entry| entry.path())),
            suffix,
            collect_files.then_some(&mut found.files),
            &mut found.unreadable,
            descend.then_some(&mut children),
        );
        for child in children {
            pending.push((child, depth + 1));
        }
    }
    found.files.sort();
    Ok(found)
}

fn has_suffix(path: &Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(suffix))
}

/// Sorts one folder's entries: subfolders are queued in `pending` (when descending) and matching files go to `files` (when collecting at this depth). An entry that cannot be read is counted in `unreadable`.
fn sort_entries(
    entries: impl Iterator<Item = io::Result<PathBuf>>,
    suffix: &str,
    mut files: Option<&mut Vec<PathBuf>>,
    unreadable: &mut usize,
    mut pending: Option<&mut Vec<PathBuf>>,
) {
    for entry in entries {
        let Ok(path) = entry else {
            *unreadable += 1;
            continue;
        };
        if path.is_dir() {
            if let Some(queue) = pending.as_deref_mut() {
                queue.push(path);
            }
        } else if let Some(out) = files.as_deref_mut()
            && has_suffix(&path, suffix)
        {
            out.push(path);
        }
    }
}

/// Streams JSONL from `reader` line by line, skipping invalid JSON lines and reusing one line buffer.
///
/// Returns `true` when finished (or stopped early by `visit`), or `false` when `on_error` is [`LineError::Abort`] and a line fails to read.
pub fn for_each_json_line(
    mut reader: impl BufRead,
    on_error: LineError,
    mut visit: impl FnMut(&Value) -> ControlFlow<()>,
) -> bool {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return true,
            Ok(_) => {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if visit(&value).is_break() {
                    return true;
                }
            }
            Err(_) => match on_error {
                LineError::Abort => return false,
                LineError::Skip => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ScratchDir;

    #[test]
    fn files_are_found_recursively_by_suffix() {
        let root = ScratchDir::new("files-by-suffix");
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("one.settings.json"), "{}").unwrap();
        fs::write(nested.join("two.settings.json"), "{}").unwrap();
        fs::write(nested.join("three.jsonl"), "").unwrap();

        let found = files_with_suffix(&root, ".settings.json").unwrap();

        let mut names: Vec<String> = found
            .files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["one.settings.json", "two.settings.json"],
            "files are found in nested folders by suffix only"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_at_depth_one_stops_before_deep_subtrees() {
        use std::os::unix::fs::PermissionsExt;

        let root = ScratchDir::new("files-subdirs");
        let child = root.join("proj");
        let deeper = child.join("sub");
        let much_deeper = deeper.join("leaf");
        fs::create_dir_all(&much_deeper).unwrap();
        fs::write(root.join("top.jsonl"), "").unwrap();
        fs::write(child.join("ok.jsonl"), "").unwrap();
        fs::write(child.join("note.txt"), "").unwrap();
        fs::write(deeper.join("nested.jsonl"), "").unwrap();
        fs::write(much_deeper.join("deep.jsonl"), "").unwrap();
        fs::set_permissions(&much_deeper, fs::Permissions::from_mode(0o000)).unwrap();

        let found = files_at_depth(&root, ".jsonl", 1);
        fs::set_permissions(&much_deeper, fs::Permissions::from_mode(0o755)).unwrap();

        let found = found.unwrap();
        assert_eq!(found.files, [child.join("ok.jsonl")]);
        assert_eq!(found.unreadable, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_reached_through_a_symlink_is_read_once() {
        let root = ScratchDir::new("symlinked-folder");
        let real = root.join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("a.jsonl"), "").unwrap();
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();

        let found = files_with_suffix(&root, ".jsonl").unwrap();

        assert_eq!(found.files.len(), 1, "found {found:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_ends_and_still_finds_the_files() {
        let root = ScratchDir::new("symlink-loop");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("a.jsonl"), "").unwrap();
        std::os::unix::fs::symlink(&root, nested.join("back")).unwrap();
        std::os::unix::fs::symlink(&nested, root.join("again")).unwrap();

        let found = files_with_suffix(&root, ".jsonl").unwrap();

        assert_eq!(found.files.len(), 1, "found {found:?}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_skipped_and_the_rest_is_still_read() {
        use std::os::unix::fs::PermissionsExt;

        let root = ScratchDir::new("unreadable-folder");
        let open = root.join("open");
        let locked = root.join("locked");
        fs::create_dir_all(&open).unwrap();
        fs::create_dir_all(&locked).unwrap();
        fs::write(open.join("a.jsonl"), "").unwrap();
        fs::write(locked.join("b.jsonl"), "").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let found = files_with_suffix(&root, ".jsonl");

        // Restore access so the scratch folder can be removed, whatever the result.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let found = found.unwrap();
        assert_eq!(
            found.files.len(),
            1,
            "the readable folder's file is still found: {found:?}"
        );
        assert_eq!(
            found.unreadable, 1,
            "the locked folder is counted as unreadable"
        );
    }

    #[test]
    fn an_unreadable_entry_is_counted_and_the_others_are_sorted() {
        let entries = vec![
            Ok(PathBuf::from("/nowhere/a.jsonl")),
            Err(io::Error::other("entry vanished")),
            Ok(PathBuf::from("/nowhere/notes.txt")),
        ];
        let mut found = Found::default();
        let mut pending = Vec::new();

        sort_entries(
            entries.into_iter(),
            ".jsonl",
            Some(&mut found.files),
            &mut found.unreadable,
            Some(&mut pending),
        );

        assert_eq!(
            found.files,
            [PathBuf::from("/nowhere/a.jsonl")],
            "only the matching file is kept"
        );
        assert_eq!(
            found.unreadable, 1,
            "the entry that could not be read is counted"
        );
        assert!(pending.is_empty(), "no subfolder is queued");
    }

    #[test]
    fn missing_root_is_an_error_that_names_the_root() {
        let err = files_with_suffix(Path::new("/nonexistent-droid-usage-root"), ".jsonl")
            .expect_err("a missing root folder is reported as an error");
        assert!(
            err.to_string().contains("/nonexistent-droid-usage-root"),
            "the error names the root folder: {err}"
        );
    }

    #[test]
    fn jsonl_line_error_modes_skip_or_abort_as_requested() {
        let data = b"{\"n\":1}\nnot json\n\n\xff\n{\"n\":2}\n";

        let mut skipped = Vec::new();
        let completed = for_each_json_line(data.as_slice(), LineError::Skip, |val| {
            if let Some(n) = val["n"].as_u64() {
                skipped.push(n);
            }
            ControlFlow::Continue(())
        });
        assert!(completed);
        assert_eq!(skipped, [1, 2]);

        let mut aborted = Vec::new();
        let completed = for_each_json_line(data.as_slice(), LineError::Abort, |val| {
            if let Some(n) = val["n"].as_u64() {
                aborted.push(n);
            }
            ControlFlow::Continue(())
        });
        assert!(!completed);
        assert_eq!(aborted, [1]);
    }
}
