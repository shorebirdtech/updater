use crate::updater::UpdateError;

// This file's job is to handle the boiler plate around locking for the
// updater thread.
// We should be able to share more of this code with similar boilerplate around
// the ResolveConfig global lock with a little refactoring.

fn updater_lock() -> &'static std::sync::Mutex<UpdaterLockState> {
    use once_cell::sync::OnceCell;
    use std::sync::Mutex;
    static INSTANCE: OnceCell<Mutex<UpdaterLockState>> = OnceCell::new();
    INSTANCE.get_or_init(|| Mutex::new(UpdaterLockState::empty()))
}

// Note: it is not OK to ever ask for the Updater lock *while* holding the
// ResolveConfig lock because the updater thread *will* block on getting the
// ResolveConfig lock while holding the Updater lock.  Allowing the inverse
// could cause a deadlock.  We could add a check for that here by doing a
// tryLock on the ResolveConfig lock and erroring out if we can't get it, but
// that would probably have false postives since it is OK for some other call to
// be holding the ResolveConfig lock while another thread asks for the Updater
// lock.
pub fn with_updater_thread_lock<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&UpdaterLockState) -> anyhow::Result<R>,
{
    println!("Waiting for updater thread lock");
    // Unlike our ResolveConfig lock, our UpdaterThread lock does not wait
    // if an updater thread is already running. We use try_lock instead
    // of lock to error out immediately.
    let lock = updater_lock().try_lock();
    match lock {
        Ok(lock) => f(&lock),
        Err(std::sync::TryLockError::WouldBlock) => {
            anyhow::bail!(UpdateError::UpdateAlreadyInProgress)
        }
        // This should never happen. Poisoning only happens if a thread panics
        // while holding the lock, and we never allow the updater thread to
        // panic.
        Err(std::sync::TryLockError::Poisoned(e)) => panic!("Updater lock poisoned: {:?}", e),
    }
}

#[derive(Debug)]
pub struct UpdaterLockState {
    // This is held by the thread doing the update, not by the thread launching
    // the update.  This is because in the case of start_update_thread, we
    // don't want to block on the calling thread while the update is running.
}

impl UpdaterLockState {
    pub fn empty() -> Self {
        Self {}
    }
}
