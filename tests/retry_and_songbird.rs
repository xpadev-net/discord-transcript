use discord_transcript::retry::{RetryPolicy, retry_with_backoff};
use discord_transcript::songbird_adapter::SsrcTracker;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[test]
fn retry_succeeds_before_max_attempts() {
    let calls = AtomicU32::new(0);
    let result: Result<u32, &'static str> = retry_with_backoff(
        RetryPolicy {
            max_attempts: 3,
            initial_delay: Duration::from_millis(1),
            backoff_multiplier: 2,
            max_delay: Duration::from_millis(2),
        },
        |_| {
            let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt < 3 {
                Err("not yet")
            } else {
                Ok(attempt)
            }
        },
    );

    assert_eq!(result.expect("retry should succeed"), 3);
}

#[test]
fn retry_returns_last_error_after_max_attempts() {
    let calls = AtomicU32::new(0);
    let result: Result<(), &'static str> = retry_with_backoff(
        RetryPolicy {
            max_attempts: 2,
            initial_delay: Duration::from_millis(1),
            backoff_multiplier: 2,
            max_delay: Duration::from_millis(2),
        },
        |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("always fail")
        },
    );
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn ssrc_tracker_maps_ssrc_to_user_id() {
    let mut tracker = SsrcTracker::new();
    tracker.update_mapping(1234, 5678);
    assert_eq!(tracker.resolve_user(1234), Some("5678"));
    assert_eq!(tracker.resolve_user(9999), None);
}
