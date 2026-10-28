// Support for restarting the running app to boot a newly installed patch
// without killing the host process ("hot restart" of production apps).
//
// The updater itself cannot restart the Dart VM: only the Flutter engine
// can tear down and relaunch the root isolate. Instead, the engine registers
// a restart handler at startup (via `shorebird_set_restart_handler` on the
// engine C surface), and `package:shorebird_code_push` requests a restart
// over FFI (via `shorebird_restart_app` on the Dart C surface). This module
// is the relay between the two.

use std::sync::Mutex;

/// Handler installed by the Flutter engine. Returns true if the engine
/// accepted the request and scheduled a restart.
pub type RestartHandler = extern "C" fn() -> bool;

static RESTART_HANDLER: Mutex<Option<RestartHandler>> = Mutex::new(None);

/// Reads past a poisoned lock instead of panicking. Both critical
/// sections here are a single assignment or copy, so poisoning means a
/// panic somewhere else left the process in a strange state — and these
/// functions are reached through `extern "C"`, where a panic aborts the
/// app rather than unwinding into a caller that could handle it.
fn handler_lock() -> std::sync::MutexGuard<'static, Option<RestartHandler>> {
    RESTART_HANDLER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Installs (or clears, with `None`) the engine's restart handler.
///
/// Clearing does not join a call already in flight: [`request_restart`]
/// copies the handler out of the lock before invoking it, so a handler
/// can still run after this returns `None`. The engine's handler has to
/// stay safe to call against a shell it has begun tearing down.
pub fn set_restart_handler(handler: Option<RestartHandler>) {
    *handler_lock() = handler;
}

/// Asks the engine to restart the app. Returns true if a handler was
/// installed and it accepted the request. Returns false when no handler is
/// registered (e.g. running against an engine without hot restart support,
/// or in a debug build where the updater is not attached to an engine).
///
/// The handler runs on the calling thread, which is whichever thread
/// asked for the restart — the Dart isolate's thread when the request
/// comes from `package:shorebird_code_push`. Tearing down that isolate
/// from this call would be tearing down the thread it is running on, so
/// the engine hops to its platform thread itself.
pub fn request_restart() -> bool {
    // Copy the handler out of the lock before invoking it so a slow or
    // re-entrant handler cannot deadlock against set_restart_handler.
    let handler = *handler_lock();
    match handler {
        Some(handler) => handler(),
        None => {
            shorebird_warn!("Restart requested, but no restart handler is registered.");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The handler registry is process-global state, so these tests are
    // serialized and each test leaves the handler cleared.

    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn accepting_handler() -> bool {
        CALL_COUNT.fetch_add(1, Ordering::SeqCst);
        true
    }

    extern "C" fn rejecting_handler() -> bool {
        CALL_COUNT.fetch_add(1, Ordering::SeqCst);
        false
    }

    #[test]
    #[serial]
    fn request_restart_without_handler_returns_false() {
        super::set_restart_handler(None);
        assert!(!super::request_restart());
    }

    #[test]
    #[serial]
    fn request_restart_invokes_handler_and_relays_result() {
        CALL_COUNT.store(0, Ordering::SeqCst);

        super::set_restart_handler(Some(accepting_handler));
        assert!(super::request_restart());
        assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 1);

        super::set_restart_handler(Some(rejecting_handler));
        assert!(!super::request_restart());
        assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 2);

        super::set_restart_handler(None);
        assert!(!super::request_restart());
        assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 2);
    }
}
