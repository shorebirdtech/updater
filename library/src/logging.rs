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

#[cfg(any(target_os = "ios", target_os = "macos"))]
pub fn init_logging() {
    let init_result = oslog::OsLogger::new("dev.shorebird")
        .level_filter(log::LevelFilter::Info)
        .init();
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
