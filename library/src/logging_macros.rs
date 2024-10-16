// Wrappers around crate::log's logging functions that prepend "[shorebird]" to the log message.

#[macro_export]
macro_rules! shorebird_info {
    // shorebird_info!("a {} event", "log")
    ($fmt:expr $(, $($arg:tt)*)?) => {
        log::info!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[macro_export]
macro_rules! shorebird_debug {
    // shorebird_debug!("a {} event", "log")
    ($fmt:expr $(, $($arg:tt)*)?) => {
        log::debug!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[macro_export]
macro_rules! shorebird_warn {
    // shorebird_warn!("a {} event", "log")
    ($fmt:expr $(, $($arg:tt)*)?) => {
        log::warn!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[macro_export]
macro_rules! shorebird_error {
    // shorebird_error!("a {} event", "log")
    ($fmt:expr $(, $($arg:tt)*)?) => {
        log::error!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}
