#[cfg(test)]
use mock_instant::global::SystemTime;

#[cfg(not(test))]
use std::time::SystemTime;

/// The number of seconds since the Unix epoch. Returns 0 if the system clock is set before the
/// Unix epoch.
pub(crate) fn unix_timestamp() -> u64 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(n) => n.as_secs(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mock_instant::global::MockClock;

    #[test]
    fn returns_duration_since_unix_epoch() {
        MockClock::set_system_time(Duration::from_secs(123));
        assert_eq!(super::unix_timestamp(), 123);
    }

    // Ideally, we'd be able to test the case where `duration_since` returns an error, but it
    // seems to only happen when system time is set before the Unix epoch, which is not possible
    // with the current implementation of `MockClock` because `set_system_time` expects a duration,
    // and a duration cannot be negative.
}
