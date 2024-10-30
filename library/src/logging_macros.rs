// Wrappers around crate::log's logging functions that prepend "[shorebird]" to the log message.
//
// See https://stackoverflow.com/questions/67087597/is-it-possible-to-use-rusts-log-info-for-tests
// for the rationale behind the use of the #[cfg(test)] attribute.

#[cfg(test)]
#[macro_export]
macro_rules! shorebird_info {
    ($fmt:expr $(, $($arg:tt)*)?) => {
        println!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[cfg(not(test))]
#[macro_export]
macro_rules! shorebird_info {
    // shorebird_info!("a {} event", "log")
    ($fmt:expr $(, $($arg:tt)*)?) => {
        log::info!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[cfg(test)]
#[macro_export]
macro_rules! shorebird_debug {
    ($fmt:expr $(, $($arg:tt)*)?) => {
        println!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[cfg(not(test))]
#[macro_export]
macro_rules! shorebird_debug {
    // shorebird_debug!("a {} event", "log")
    ($fmt:expr $(, $($arg:tt)*)?) => {
        log::debug!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[cfg(test)]
#[macro_export]
macro_rules! shorebird_warn {
    ($fmt:expr $(, $($arg:tt)*)?) => {
        println!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[cfg(not(test))]
#[macro_export]
macro_rules! shorebird_warn {
    // shorebird_warn!("a {} event", "log")
    ($fmt:expr $(, $($arg:tt)*)?) => {
        log::warn!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[cfg(test)]
#[macro_export]
macro_rules! shorebird_error {
    ($fmt:expr $(, $($arg:tt)*)?) => {
        println!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}

#[cfg(not(test))]
#[macro_export]
macro_rules! shorebird_error {
    // shorebird_error!("a {} event", "log")
    ($fmt:expr $(, $($arg:tt)*)?) => {
        log::error!(concat!("[shorebird] ", $fmt), $($($arg)*)?)
    };
}
