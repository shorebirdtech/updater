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
    // I could not figure out how to get fancier logging set up on iOS
    // but logging to stderr seems to work.
    simple_logging::log_to(std::io::stderr(), log::LevelFilter::Info);
    debug!("Logging initialized");
}

#[cfg(all(not(target_os = "android"), not(target_os = "ios")))]
pub fn init_logging() {
    // Nothing to do on non-Android, non-iOS platforms.
}
