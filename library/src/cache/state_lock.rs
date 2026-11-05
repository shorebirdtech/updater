// Cross-process exclusion for the updater's state files.

use std::{
    fs::{File, OpenOptions},
    path::Path,
    time::{Duration, Instant},
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
/// dies, so a crash can't leave it held. A holder that is alive but stuck
/// can't wedge this process either: see [`ACQUIRE_TIMEOUT`]. The lock file itself is never
/// deleted: a second process locking a recreated file would not exclude the
/// first.
pub struct StateLock {
    _file: Option<File>,
}

/// How long [`StateLock::acquire`] waits for another process to let go.
///
/// A healthy holder keeps the lock only for a few milliseconds of file I/O.
/// One that holds it longer is stuck: suspended, stopped in a debugger, or
/// on a stalled disk. Waiting on it without bound would hang this process's
/// updater, and at startup that's the Android main thread. So after this
/// long the caller goes on unlocked, as every updater did before the lock.
/// Engine startup takes the lock about five times, so a stuck holder costs a
/// launch at most a few seconds, under Android's 5s input-dispatch ANR.
const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(500);

impl StateLock {
    /// Waits up to [`ACQUIRE_TIMEOUT`] for this process to hold the lock. If
    /// it times out, or the lock file can't be opened or locked, logs and
    /// returns a lock that holds nothing: state access goes on as it did
    /// before there was a lock rather than failing or hanging.
    pub fn acquire(cache_dir: &Path) -> Self {
        Self::acquire_within(cache_dir, ACQUIRE_TIMEOUT)
    }

    pub(crate) fn acquire_within(cache_dir: &Path, timeout: Duration) -> Self {
        match Self::try_acquire(cache_dir, timeout) {
            Ok(file) => Self { _file: Some(file) },
            Err(e) => {
                shorebird_warn!("Failed to lock state in {:?}: {:?}", cache_dir, e);
                Self { _file: None }
            }
        }
    }

    fn try_acquire(cache_dir: &Path, timeout: Duration) -> std::io::Result<File> {
        std::fs::create_dir_all(cache_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(cache_dir.join(LOCK_FILE_NAME))?;
        // Neither flock nor LockFileEx takes a timeout, so poll the
        // non-blocking form with a short backoff, as SQLite's default busy
        // handler does.
        let deadline = Instant::now() + timeout;
        let mut backoff = Duration::from_millis(1);
        while !try_lock_exclusive(&file)? {
            let now = Instant::now();
            if now >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("another process held {LOCK_FILE_NAME} for over {timeout:?}"),
                ));
            }
            std::thread::sleep(backoff.min(deadline - now));
            backoff = (backoff * 2).min(Duration::from_millis(20));
        }
        Ok(file)
    }
}

/// Takes the lock if it's free. `Ok(false)` means another open holds it.
// libc rather than `File::try_lock` on Unix: std's Android support for it is
// recent (absent through 1.97), and the engine builds with whatever stable
// is installed. Windows has had `File::try_lock` since it was stabilized
// (1.89).
#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    loop {
        // SAFETY: the fd is valid for as long as `file` is borrowed.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let err = std::io::Error::last_os_error();
        match err.kind() {
            std::io::ErrorKind::Interrupted => continue,
            std::io::ErrorKind::WouldBlock => return Ok(false),
            _ => return Err(err),
        }
    }
}

#[cfg(windows)]
fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
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
            let second = StateLock::acquire_within(&dir, Duration::from_secs(60));
            tx.send(second._file.is_some()).unwrap();
        });
        // Blocked while `first` is held. Only the absence of a send is
        // checked here, so a slow machine can't fail it.
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        assert!(rx.recv_timeout(Duration::from_secs(60)).unwrap());
        waiter.join().unwrap();
    }

    #[test]
    fn gives_up_unlocked_when_holder_never_lets_go() {
        let tmp = TempDir::new().unwrap();
        let _held = StateLock::acquire(tmp.path());
        // `_held` is never dropped while this waits, so the outcome doesn't
        // depend on timing: only how long the wait takes does.
        let second = StateLock::acquire_within(tmp.path(), Duration::from_millis(10));
        assert!(second._file.is_none());
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
