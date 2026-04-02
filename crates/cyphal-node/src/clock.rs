//! Embassy-time-based [`Clock`] implementation for the Cyphal stack.

use canadensis::core::time::{Clock, Microseconds32};
use embassy_time::Instant;

/// A [`Clock`] implementation that wraps Embassy's monotonic timer.
///
/// Provides microsecond-resolution timestamps to the `canadensis` Cyphal stack
/// by converting [`embassy_time::Instant::now()`] to 32-bit microseconds.
pub struct TimerClock;

impl Clock for TimerClock {
    fn now(&mut self) -> Microseconds32 {
        Microseconds32::from_ticks(Instant::now().as_micros() as u32)
    }
}
