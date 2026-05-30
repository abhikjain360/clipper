//! Filesystem rollback guard for file writes that must be undone unless an
//! operation succeeds.
//!
//! [`FsTransaction`] tracks files as they are created and removes them when it
//! is dropped, unless [`commit`](FsTransaction::commit) is called first. Files
//! are written to their final paths immediately, so this does not provide
//! isolation — only cleanup. It is the on-disk analogue of a database
//! transaction's rollback: the side effects that live in the filesystem rather
//! than in the database get undone on the same failure paths, without the
//! caller having to track and delete the paths by hand.
//!
//! ```no_run
//! # async fn run() -> std::io::Result<()> {
//! use clipper_fs_txn::FsTransaction;
//!
//! let mut staged = FsTransaction::new();
//! staged.write_new("/tmp/blob.bin", b"ciphertext").await?;
//! // ... do fallible work; any early return here removes the file ...
//! staged.commit(); // success: keep the file
//! # Ok(())
//! # }
//! ```

use std::{io, path::PathBuf};

use tokio::io::AsyncWriteExt;
use tracing::warn;

/// A set of files created as a unit. Dropping it without calling
/// [`commit`](Self::commit) removes every file it created, on a best-effort
/// basis.
#[derive(Debug, Default)]
#[must_use = "dropping an FsTransaction without `commit` deletes the staged files"]
pub struct FsTransaction {
    paths: Vec<PathBuf>,
}

impl FsTransaction {
    /// Create an empty transaction tracking no files.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create `path` (which must not already exist), write `data` to it, and
    /// start tracking it for rollback.
    ///
    /// The file is tracked the moment it is created, so a failure midway
    /// through the write is still rolled back when this guard is dropped. If
    /// the path already exists the call fails with [`io::ErrorKind::AlreadyExists`]
    /// and the pre-existing file is left untouched and untracked.
    pub async fn write_new(&mut self, path: impl Into<PathBuf>, data: &[u8]) -> io::Result<()> {
        let path = path.into();
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        // Own it from here: record before writing so a partial write rolls back.
        self.paths.push(path);
        file.write_all(data).await?;
        file.flush().await
    }

    /// Track an externally created `path` for rollback, for callers that write
    /// files themselves (e.g. streaming to a temp path and renaming).
    pub fn track(&mut self, path: impl Into<PathBuf>) {
        self.paths.push(path.into());
    }

    /// Keep every tracked file. Consumes the guard so [`Drop`] removes nothing.
    pub fn commit(mut self) {
        self.paths.clear();
    }
}

impl Drop for FsTransaction {
    fn drop(&mut self) {
        for path in std::mem::take(&mut self.paths) {
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != io::ErrorKind::NotFound
            {
                warn!(
                    path = %path.display(),
                    error = %error,
                    "Best-effort staged file rollback failed",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn commit_keeps_written_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");

        let mut txn = FsTransaction::new();
        txn.write_new(&path, b"hello").await.unwrap();
        assert!(path.exists());
        txn.commit();

        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn drop_without_commit_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");

        {
            let mut txn = FsTransaction::new();
            txn.write_new(&path, b"hello").await.unwrap();
            assert!(path.exists());
        }

        assert!(!path.exists());
    }

    #[tokio::test]
    async fn rolls_back_written_and_tracked_paths() {
        let dir = tempfile::tempdir().unwrap();
        let written = dir.path().join("written.bin");
        let external = dir.path().join("external.bin");
        std::fs::write(&external, b"external").unwrap();

        {
            let mut txn = FsTransaction::new();
            txn.write_new(&written, b"x").await.unwrap();
            txn.track(&external);
            assert!(written.exists() && external.exists());
        }

        assert!(!written.exists());
        assert!(!external.exists());
    }

    #[tokio::test]
    async fn write_new_rejects_existing_path_without_tracking_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, b"pre-existing").unwrap();

        let mut txn = FsTransaction::new();
        let err = txn.write_new(&path, b"x").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);

        // A file we did not create must not be removed on rollback.
        drop(txn);
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"pre-existing");
    }
}
