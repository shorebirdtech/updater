// Cross-process exclusion for the updater's state files.

use std::{
    fs::{File, OpenOptions},
    path::Path,
};

const LOCK_FILE_NAME: &str = "state.lock";

/// An exclusive advisory lock on `{cache_dir}/state.lock`, held until drop.
///
/// The global config `Mutex` serializes state access within one process, but
/// an Android app can run a second process (`android:process` services, a
/// separate engine for background work) with its own updater over the same
/// `cache_dir`. Holding this across load, mutate and save makes each
/// read-modify-write of the state files exclusive across processes too.
///
/// The lock is released when the file is closed, including when the process
/// dies, so a crash can't leave it held. The lock file itself is never
/// deleted: a second process locking a recreated file would not exclude the
/// first.
pub struct StateLock {
    _file: Option<File>,
}

impl StateLock {
    /// Blocks until this process holds the lock. If the lock file can't be
    /// opened or locked, logs and returns an unheld lock: state access goes
    /// on as it did before there was a lock rather than failing.
    pub fn acquire(cache_dir: &Path) -> Self {
        match Self::try_acquire(cache_dir) {
            Ok(file) => Self { _file: Some(file) },
            Err(e) => {
                shorebird_warn!("Failed to lock state in {:?}: {:?}", cache_dir, e);
                Self { _file: None }
            }
        }
    }

    fn try_acquire(cache_dir: &Path) -> std::io::Result<File> {
        std::fs::create_dir_all(cache_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(cache_dir.join(LOCK_FILE_NAME))?;
        lock_exclusive(&file)?;
        Ok(file)
    }
}

// libc rather than `File::lock`, whose Android support is newer than the
// toolchains the engine may build with.
#[cfg(unix)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    loop {
        // SAFETY: the fd is valid for as long as `file` is borrowed.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(windows)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    file.lock()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn second_lock_waits_for_first_to_drop() {
        let tmp = TempDir::new().unwrap();
        let first = StateLock::acquire(tmp.path());
        assert!(first._file.is_some());

        let (tx, rx) = mpsc::channel();
        let dir = tmp.path().to_path_buf();
        let waiter = std::thread::spawn(move || {
            // A separate open, so a separate lock, as in another process.
            let _second = StateLock::acquire(&dir);
            tx.send(()).unwrap();
        });
        // Blocked while `first` is held. Only the absence of a send is
        // checked here, so a slow machine can't fail it.
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(30)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn creates_missing_cache_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("a/b");
        let lock = StateLock::acquire(&dir);
        assert!(lock._file.is_some());
        assert!(dir.join(LOCK_FILE_NAME).exists());
    }
}
