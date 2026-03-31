use embassy_time::{Duration, Instant};
use portable_atomic::{AtomicU64, Ordering};

static IDENTIFY_UNTIL_TICKS: AtomicU64 = AtomicU64::new(0);

pub fn trigger_identify(duration: Duration) {
    let until = Instant::now() + duration;
    IDENTIFY_UNTIL_TICKS.store(until.as_ticks(), Ordering::Release);
}

pub fn is_identifying() -> bool {
    Instant::now().as_ticks() < IDENTIFY_UNTIL_TICKS.load(Ordering::Acquire)
}

pub fn wheel_color(pos: u8) -> (u8, u8, u8) {
    let pos = 255u8.wrapping_sub(pos);
    if pos < 85 {
        return (
            255u8.wrapping_sub(pos.saturating_mul(3)),
            0,
            pos.saturating_mul(3),
        );
    }

    if pos < 170 {
        let shifted = pos - 85;
        return (
            0,
            shifted.saturating_mul(3),
            255u8.wrapping_sub(shifted.saturating_mul(3)),
        );
    }

    let shifted = pos - 170;
    (
        shifted.saturating_mul(3),
        255u8.wrapping_sub(shifted.saturating_mul(3)),
        0,
    )
}
