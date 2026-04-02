use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoStopState {
    grace_period: Duration,
    empty_since_ms: Option<u64>,
}

impl AutoStopState {
    pub fn new(grace_period: Duration) -> Self {
        Self {
            grace_period,
            empty_since_ms: None,
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
            return AutoStopSignal::Pending;
        }

        if self.empty_since_ms.take().is_some() {
            return AutoStopSignal::Cancelled;
        }

        AutoStopSignal::Pending
    }

    pub fn tick(&mut self, now_ms: u64) -> AutoStopSignal {
        let Some(empty_since_ms) = self.empty_since_ms else {
            return AutoStopSignal::Pending;
        };

        let elapsed = now_ms.saturating_sub(empty_since_ms);
        if elapsed >= self.grace_period.as_millis() as u64 {
            self.empty_since_ms = None;
            return AutoStopSignal::Trigger;
        }

        AutoStopSignal::Pending
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStopSignal {
    Pending,
    Cancelled,
    Trigger,
}
