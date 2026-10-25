//! The host monotonic clock shared by every module in the host process.
//!
//! Video packets leave the video service stamped on this clock, so a
//! downstream module can measure how long the host held a frame by reading the
//! same clock. It is CLOCK_MONOTONIC on Linux and the performance counter on
//! Windows, the clocks `std::time::Instant` uses there.

/// Microseconds on the host monotonic clock.
#[cfg(unix)]
pub fn host_monotonic_micros() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a writable timespec for this call; CLOCK_MONOTONIC is
    // always available on the supported Unix targets.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    if result != 0 {
        return 0;
    }
    (now.tv_sec as u64)
        .saturating_mul(1_000_000)
        .saturating_add(now.tv_nsec as u64 / 1_000)
}

/// Microseconds on the host monotonic clock.
#[cfg(windows)]
pub fn host_monotonic_micros() -> u64 {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    let (mut counter, mut frequency) = (0i64, 0i64);
    // SAFETY: both pointers name writable integers for these calls, which
    // cannot fail on supported Windows versions.
    let read = unsafe {
        QueryPerformanceCounter(&mut counter) != 0 && QueryPerformanceFrequency(&mut frequency) != 0
    };
    if !read || counter < 0 || frequency <= 0 {
        return 0;
    }
    (counter as u128 * 1_000_000 / frequency as u128) as u64
}

/// Host processing latency in Moonlight's 0.1 ms frame-header units, from a
/// frame stamped on the host clock at capture to `now`. Zero means unknown: a
/// timestamp from the future or more than five seconds old did not come from
/// this clock.
pub fn host_processing_latency_tenths_ms(captured_us: u64, now_us: u64) -> u16 {
    match now_us.checked_sub(captured_us) {
        Some(elapsed) if elapsed <= 5_000_000 => ((elapsed + 50) / 100) as u16,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_clock_advances() {
        let first = host_monotonic_micros();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = host_monotonic_micros();
        assert!(first > 0 && second >= first + 1_000, "{first} {second}");
    }

    #[test]
    fn latency_rounds_to_tenths_and_rejects_foreign_timestamps() {
        assert_eq!(host_processing_latency_tenths_ms(1_000, 1_000), 0);
        assert_eq!(host_processing_latency_tenths_ms(1_000, 5_249), 42);
        assert_eq!(host_processing_latency_tenths_ms(1_000, 5_250), 43);
        assert_eq!(host_processing_latency_tenths_ms(0, 5_000_000), 50_000);
        assert_eq!(host_processing_latency_tenths_ms(2_000, 1_000), 0);
        assert_eq!(host_processing_latency_tenths_ms(0, 5_000_001), 0);
    }
}
