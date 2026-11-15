use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use anyhow::{Context, Result};
use fs2::FileExt;

/// Exclusive advisory lock over a pool directory. Dropping it releases the
/// lock and the kernel drops it if the process dies, so no stale lock files
/// need cleanup.
pub struct PoolLock {
    file: File,
}

impl PoolLock {
    /// `Ok(None)` means another process holds the lock.
    pub fn acquire(dir: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(dir)
            .with_context(|| format!("creating pool directory {}", dir.display()))?;
        let path = dir.join(".lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { file })),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error).with_context(|| format!("locking {}", path.display())),
        }
    }
}

impl Drop for PoolLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let first = PoolLock::acquire(temp.path()).unwrap();
        assert!(first.is_some());
        assert!(PoolLock::acquire(temp.path()).unwrap().is_none());
        drop(first);
        assert!(PoolLock::acquire(temp.path()).unwrap().is_some());
    }
}
