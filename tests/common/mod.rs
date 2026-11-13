//! Scratch folders shared by the integration tests.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// An empty folder for one test, named for this process and `label`. It is removed again on drop.
#[derive(Debug)]
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    /// Makes an empty folder, after clearing any leftover from an earlier run of the same test.
    ///
    /// # Panics
    ///
    /// Panics if the folder cannot be created.
    #[must_use]
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("droid-usage-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
