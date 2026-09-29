// Support for restarting the running app to boot a newly installed patch
// without killing the host process ("hot restart" of production apps).
//
// The updater itself cannot restart the Dart VM — only the Flutter engine
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

/// Installs (or clears, with `None`) the engine's restart handler.
pub fn set_restart_handler(handler: Option<RestartHandler>) {
    let mut lock = RESTART_HANDLER.lock().unwrap();
    *lock = handler;
}

/// Asks the engine to restart the app. Returns true if a handler was
/// installed and it accepted the request. Returns false when no handler is
/// registered (e.g. running against an engine without hot restart support,
/// or in a debug build where the updater is not attached to an engine).
pub fn request_restart() -> bool {
    // Copy the handler out of the lock before invoking it so a slow or
    // re-entrant handler cannot deadlock against set_restart_handler.
    let handler = *RESTART_HANDLER.lock().unwrap();
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
    #[serial(restart_handler)]
    fn request_restart_without_handler_returns_false() {
        super::set_restart_handler(None);
        assert!(!super::request_restart());
    }

    #[test]
    #[serial(restart_handler)]
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
