//! Test-only helpers shared by the unit tests: scratch folders and fixtures.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A scratch path for one test. It is cleared when made and removed again on drop, so a failed test leaves nothing behind.
#[derive(Debug)]
pub(crate) struct ScratchDir(PathBuf);

impl ScratchDir {
    /// A path in the temp folder, named for this process and `label`. Nothing is created, so a test can make a folder or a file there.
    pub(crate) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("droid-usage-{}-{label}", std::process::id()));
        remove(&path);
        Self(path)
    }
}

impl Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        remove(&self.0);
    }
}

/// Removes the folder or file at `path`, if there is one.
fn remove(path: &Path) {
    let _ = fs::remove_dir_all(path).or_else(|_| fs::remove_file(path));
}

/// The price table as the live source served it on 2026-10-25, cut to the models the report prices and a few embedding models.
pub(crate) const RECORDED_TABLE: &str = include_str!("../tests/recorded/litellm-sample.json");

/// A one-model price table, for tests that need a table to save.
pub(crate) const SAMPLE_TABLE: &str = r#"{"gpt-5.6-sol": {"mode": "chat", "input_cost_per_token": 2e-06, "output_cost_per_token": 8e-06}}"#;

/// A one-model models page with a promotion, for tests that need a page to save.
pub(crate) const SAMPLE_PAGE: &str = "| GPT-5.6 Sol<sup>†</sup> | `gpt-5.6-sol` | 1.6× | `none` |

<sup>†</sup> Promotional pricing. GPT-5.6 Sol bills at 1.6× through November 22, 2026, then returns to 2×.";
