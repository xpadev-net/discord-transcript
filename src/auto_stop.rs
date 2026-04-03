use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoStopState {
    grace_period: Duration,
    empty_since_ms: Option<u64>,
    /// True while a grace-period timer task is in flight.
    /// Prevents spawning multiple concurrent timer tasks.
    timer_active: bool,
}

impl AutoStopState {
    pub fn new(grace_period: Duration) -> Self {
        Self {
            grace_period,
            empty_since_ms: None,
            timer_active: false,
        }
    }

    pub fn on_non_bot_member_count_changed(
        &mut self,
        non_bot_member_count: usize,
        now_ms: u64,
    ) -> AutoStopSignal {
        if non_bot_member_count == 0 {
            if self.empty_since_ms.is_none() {
                self.empty_since_ms = Some(now_ms);
            }
            // If a timer task is already in flight, don't request another one.
            if self.timer_active {
                return AutoStopSignal::AlreadyWaiting;
            }
            return AutoStopSignal::Pending;
        }

        // Members returned — cancel any pending grace period.
        self.timer_active = false;
        if self.empty_since_ms.take().is_some() {
            return AutoStopSignal::Cancelled;
        }

        AutoStopSignal::Pending
    }

    /// Mark that a timer task has been spawned for this grace period.
    pub fn mark_timer_active(&mut self) {
        self.timer_active = true;
    }

    /// Called when the timer task completes (regardless of outcome).
    pub fn clear_timer_active(&mut self) {
        self.timer_active = false;
    }

    pub fn tick(&mut self, now_ms: u64) -> AutoStopSignal {
        let Some(empty_since_ms) = self.empty_since_ms else {
            return AutoStopSignal::Pending;
        };

        let elapsed = now_ms.saturating_sub(empty_since_ms);
        let grace_ms = u64::try_from(self.grace_period.as_millis()).unwrap_or(u64::MAX);
        if elapsed >= grace_ms {
            self.empty_since_ms = None;
            return AutoStopSignal::Trigger;
        }

        AutoStopSignal::Pending
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStopSignal {
    Pending,
    /// A timer task is already in flight — do not spawn another.
    AlreadyWaiting,
    Cancelled,
    Trigger,
}
