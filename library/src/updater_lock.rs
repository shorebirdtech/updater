// This file's job is to handle the boiler plate around locking for the
// updater thread.
// We should be able to share more of this code with similar boilerplate around
// the ResolveConfig global lock with a little refactoring.

#[cfg(test)]
use std::cell::RefCell;

#[cfg(not(test))]
fn updater_lock() -> &'static std::sync::Mutex<UpdaterLockState> {
    use once_cell::sync::OnceCell;
    use std::sync::Mutex;
    static INSTANCE: OnceCell<Mutex<UpdaterLockState>> = OnceCell::new();
    INSTANCE.get_or_init(|| Mutex::new(UpdaterLockState::empty()))
}

#[cfg(test)]
thread_local!(static THREAD_UPDATER_LOCK_STATE: RefCell<UpdaterLockState> = RefCell::new(UpdaterLockState::empty()));

// Note: it is not OK to ever ask for the Updater lock *while* holding the
// ResolveConfig lock. That could cause a deadlock.  We could add a
// check for that here by doing a tryLock on the ResolveConfig lock
// and erroring out if we can't get it, but that would probably have false
// postives since it is OK for some other call to be holding the ResolveConfig
// lock while another thread asks for the Updater lock.
#[cfg(not(test))]
pub fn with_updater_thread_lock<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&UpdaterLockState) -> anyhow::Result<R>,
{
    // Unlike our ResolveConfig lock, our UpdaterThread lock does not wait
    // if an updater thread is already running. We use try_lock instead
    // of lock to error out immediately.
    let lock = updater_lock().try_lock()?;
    f(&lock)
}

#[cfg(test)]
pub fn with_updater_thread_lock<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&UpdaterLockState) -> anyhow::Result<R>,
{
    // Rust unit tests run on multiple threads in parallel, so we use
    // a per-thread UpdaterLockState to allow multiple unit tests
    // to run in parallel without blocking on a per-process lock.
    // This still works with updater lock because we only hold it from
    // the thread starting the updater thread, not the updater thread itself.
    THREAD_UPDATER_LOCK_STATE.with(|thread_lock| {
        let thread_lock = thread_lock.borrow();
        f(&thread_lock)
    })
}

#[derive(Debug)]
pub struct UpdaterLockState {
    // This is held by the thread which launches the updater thread not by the
    // updater thread itself. It's only used to prevent a second thread from
    // launching a new updater thread. We could put state here if we need to.
    // If we do, we will need to clean it up between invocations.
}

impl UpdaterLockState {
    pub fn empty() -> Self {
        Self {}
    }
}
