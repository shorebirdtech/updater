#[cfg(target_os = "android")]
pub fn init_logging() {
    log_panics::init();

    android_logger::init_once(
        android_logger::Config::default()
            // `flutter` tool ignores non-flutter tagged logs.
            .with_tag("flutter")
            .with_max_level(log::LevelFilter::Info),
    );
    debug!("Logging initialized");
}

#[cfg(target_os = "ios")]
pub fn init_logging() {
    let init_result = oslog::OsLogger::new("dev.shorebird")
        .level_filter(log::LevelFilter::Info)
        .init();
    match init_result {
        Ok(_) => debug!("Logging initialized"),
        Err(e) => error!("Failed to initialize logging: {}", e),
    }
}

#[cfg(all(not(target_os = "android"), not(target_os = "ios")))]
pub fn init_logging() {
    // Nothing to do on non-Android, non-iOS platforms.
}
