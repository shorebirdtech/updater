use crate::updater::UpdateError;

// This file's job is to handle the boilerplate around locking for the
// updater thread.
// We lock because it doesn't make a lot of sense to ask for multiple updates
// at once.  We have a *separate* lock from UpdateConfig because we want to
// only guard against multiple updates at once, not against multiple threads
// trying to read the config at once.  We also want to allow multiple threads
// to read the config at once while an update is running.
// We could share code with config.rs which does similar for UpdateConfig.
fn updater_lock() -> &'static std::sync::Mutex<UpdaterLockState> {
    use once_cell::sync::OnceCell;
    use std::sync::Mutex;
    static INSTANCE: OnceCell<Mutex<UpdaterLockState>> = OnceCell::new();
    INSTANCE.get_or_init(|| Mutex::new(UpdaterLockState::empty()))
}

// Note: it is not OK to ever ask for the Updater lock *while* holding the
// UpdateConfig lock because the updater thread *will* block on getting the
// UpdateConfig lock while holding the Updater lock.  Allowing the inverse could
// cause a deadlock.  We could add a check for that here by doing a tryLock on
// the UpdateConfig lock and erroring out if we can't get it, but that would
// probably have false positives since it is OK for some other call to be
// holding the UpdateConfig lock while another thread asks for the Updater lock.
pub fn with_updater_thread_lock<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&UpdaterLockState) -> anyhow::Result<R>,
{
    // Unlike our UpdateConfig lock, our UpdaterThread lock does not wait
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
        Err(std::sync::TryLockError::Poisoned(e)) => {
            panic!("Updater lock poisoned: {e:?}")
        }
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
