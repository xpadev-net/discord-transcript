use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub backoff_multiplier: u32,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_millis(200),
            backoff_multiplier: 2,
            max_delay: Duration::from_secs(5),
        }
    }
}

/// Compute a jitter-adjusted delay: ±25% of the base delay.
/// Uses a simple deterministic hash of the attempt number to spread
/// concurrent retries across time without requiring a PRNG dependency.
fn jittered_delay(base: Duration, attempt: u32) -> Duration {
    // Knuth multiplicative hash to spread attempt numbers across 0..99.
    let hash = (attempt.wrapping_mul(2654435761)) % 100;
    // Map hash 0..99 → multiplier 0.75..1.25
    let factor = 75 + (hash / 2); // 75..124
    base.mul_f64(factor as f64 / 100.0)
}

/// Retry an operation with exponential backoff and jitter.
///
/// **Note:** This function uses `std::thread::sleep` and is intended for use
/// inside `tokio::task::block_in_place` or `spawn_blocking` contexts where the
/// caller has already signalled to the tokio runtime that blocking is expected.
pub fn retry_with_backoff<T, E, F>(policy: RetryPolicy, mut operation: F) -> Result<T, E>
where
    F: FnMut(u32) -> Result<T, E>,
{
    let mut delay = policy.initial_delay;
    let mut attempt = 1u32;

    loop {
        match operation(attempt) {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= policy.max_attempts {
                    return Err(err);
                }

                thread::sleep(jittered_delay(delay, attempt));
                delay = (delay * policy.backoff_multiplier).min(policy.max_delay);
                attempt += 1;
            }
        }
    }
}
