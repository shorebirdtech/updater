#[cfg(target_os = "android")]
pub fn init_logging() {
    use std::sync::Once;

    static START: Once = Once::new();

    // Wrapping the logger initialization in a Once ensures that it is only
    // initialized once, even if this function is called multiple times.
    // This just avoids a warning message from the android_logger crate.
    START.call_once(|| {
        log_panics::init();

        android_logger::init_once(
            android_logger::Config::default()
                // `flutter` tool ignores non-flutter tagged logs.
                .with_tag("flutter")
                .with_max_level(log::LevelFilter::Debug),
        );
        debug!("Logging initialized");
    });
}

#[cfg(not(target_os = "android"))]
pub fn init_logging() {
    // Nothing to do on non-Android platforms.
    // Eventually iOS/MacOS may need something here.
}
