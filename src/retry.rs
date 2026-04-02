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

                thread::sleep(delay);
                delay = (delay * policy.backoff_multiplier).min(policy.max_delay);
                attempt += 1;
            }
        }
    }
}
