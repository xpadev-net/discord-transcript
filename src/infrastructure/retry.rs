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
///
/// `seed` should be unique per call-site (e.g. a thread id or pointer hash)
/// so that concurrent callers with the same `attempt` number produce different
/// delays — preventing thundering-herd synchronisation.
fn jittered_delay(base: Duration, attempt: u32, seed: u64) -> Duration {
    // Combine attempt and per-caller seed before hashing.
    let mixed = (attempt as u64).wrapping_add(seed).wrapping_mul(2654435761);
    let hash = (mixed % 100) as u32;
    // Map hash 0..99 → factor 75..125 (inclusive), i.e. multiplier 0.75..=1.25.
    let factor = 75 + (hash * 50) / 99; // 75..=125
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
    // Per-invocation seed: use the stack address of a local variable as a cheap
    // source of per-call-site entropy.  Different threads have different stacks,
    // so concurrent callers will get different seeds.
    let anchor: u32 = 0;
    let seed = (&anchor as *const u32 as usize) as u64;

    // Guard against zero multiplier which would collapse the delay to zero and
    // produce a hot retry loop.
    let multiplier = if policy.backoff_multiplier == 0 {
        1
    } else {
        policy.backoff_multiplier
    };

    let mut delay = policy.initial_delay;
    let mut attempt = 1u32;

    loop {
        match operation(attempt) {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= policy.max_attempts {
                    return Err(err);
                }

                thread::sleep(jittered_delay(delay, attempt, seed));
                let next_delay = delay.checked_mul(multiplier).unwrap_or(policy.max_delay);
                delay = next_delay.min(policy.max_delay);
                attempt += 1;
            }
        }
    }
}
