#[cfg(target_os = "android")]
pub fn init_logging() {
    log_panics::init();

    android_logger::init_once(
        android_logger::Config::default()
            // `flutter` tool ignores non-flutter tagged logs.
            .with_tag("flutter")
            .with_max_level(log::LevelFilter::Info),
    );
    shorebird_debug!("Logging initialized");
}

/// Sends `log` records to os_log under the `dev.shorebird` subsystem, one
/// category per log target (module), as oslog's own `OsLogger` does.
#[cfg(any(target_os = "ios", target_os = "macos"))]
struct OsLogLogger {
    categories: std::sync::Mutex<std::collections::HashMap<String, oslog::OsLog>>,
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
impl log::Log for OsLogLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            log::Level::Trace => oslog::Level::Debug,
            log::Level::Debug => oslog::Level::Info,
            log::Level::Info => oslog::Level::Default,
            log::Level::Warn => oslog::Level::Error,
            log::Level::Error => oslog::Level::Fault,
        };
        let message = record.args().to_string();
        let mut categories = match self.categories.lock() {
            Ok(categories) => categories,
            Err(poisoned) => poisoned.into_inner(),
        };
        categories
            .entry(record.target().to_owned())
            .or_insert_with(|| oslog::OsLog::new("dev.shorebird", record.target()))
            .with_level(level, &message);
    }

    fn flush(&self) {}
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
pub fn init_logging() {
    log::set_max_level(log::LevelFilter::Info);
    let init_result = log::set_boxed_logger(Box::new(OsLogLogger {
        categories: std::sync::Mutex::new(std::collections::HashMap::new()),
    }));
    match init_result {
        Ok(_) => shorebird_debug!("Logging initialized"),
        Err(e) => shorebird_error!("Failed to initialize logging: {}", e),
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn init_logging() {
    let _ = simple_logger::SimpleLogger::new().init();
}

#[cfg(all(
    not(target_os = "android"),
    not(target_os = "ios"),
    not(target_os = "linux"),
    not(target_os = "macos"),
    not(target_os = "windows")
))]
pub fn init_logging() {
    // Nothing to do on non-Android, non-iOS platforms.
}
