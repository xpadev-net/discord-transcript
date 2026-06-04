use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoStopState {
    grace_period: Duration,
    meeting_id: Option<String>,
    empty_since: Option<Instant>,
    /// True while a grace-period timer task is in flight.
    /// Prevents spawning multiple concurrent timer tasks.
    timer_active: bool,
    timer_generation: u64,
}

impl AutoStopState {
    pub fn new(grace_period: Duration) -> Self {
        Self::new_for_meeting(grace_period, None)
    }

    pub fn new_for_meeting(grace_period: Duration, meeting_id: Option<String>) -> Self {
        Self {
            grace_period,
            meeting_id,
            empty_since: None,
            timer_active: false,
            timer_generation: 0,
        }
    }

    pub fn meeting_id(&self) -> Option<&str> {
        self.meeting_id.as_deref()
    }

    pub fn belongs_to_meeting(&self, meeting_id: &str) -> bool {
        match self.meeting_id.as_deref() {
            None => true,
            Some(current) => current == meeting_id,
        }
    }

    /// Notify the state that the non-bot member count has changed.
    ///
    /// Returns [`AutoStopSignal::StartTimer`] exactly once per empty-channel
    /// episode.  Mutual exclusion is provided by the caller's lock (e.g. the
    /// tokio `Mutex` in `runtime.rs`), so two concurrent callers can never
    /// both receive `StartTimer`.
    pub fn on_non_bot_member_count_changed(
        &mut self,
        non_bot_member_count: usize,
    ) -> AutoStopSignal {
        if non_bot_member_count == 0 {
            if self.empty_since.is_none() {
                self.empty_since = Some(Instant::now());
            }
            if self.timer_active {
                return AutoStopSignal::AlreadyWaiting;
            }
            self.timer_active = true;
            self.timer_generation = self.timer_generation.saturating_add(1);
            return AutoStopSignal::StartTimer;
        }

        self.timer_active = false;
        if self.empty_since.take().is_some() {
            return AutoStopSignal::Cancelled;
        }

        AutoStopSignal::Idle
    }

    /// Called when the timer task completes (regardless of outcome).
    pub fn clear_timer_active(&mut self) {
        self.timer_active = false;
    }

    pub fn clear_timer_active_for_generation(&mut self, generation: u64) {
        if self.timer_generation == generation {
            self.timer_active = false;
        }
    }

    pub fn timer_generation(&self) -> u64 {
        self.timer_generation
    }

    /// Re-arm the empty episode after a failed stop attempt so a later timer
    /// can retry while the channel is still empty.
    pub fn retry_after_failed_stop(&mut self) {
        self.empty_since = Some(Instant::now());
        self.timer_active = true;
    }

    pub fn tick(&mut self) -> AutoStopSignal {
        let Some(empty_since) = self.empty_since else {
            return AutoStopSignal::Idle;
        };

        if empty_since.elapsed() >= self.grace_period {
            self.empty_since = None;
            return AutoStopSignal::Trigger;
        }

        AutoStopSignal::Idle
    }

    #[doc(hidden)]
    pub fn set_empty_since_elapsed_for_test(&mut self, elapsed: Duration) {
        self.empty_since = Some(Instant::now() - elapsed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStopSignal {
    /// No action needed (channel occupied, or timer not yet elapsed).
    Idle,
    /// The caller should spawn a grace-period timer task.
    /// `timer_active` has already been set — do **not** call any additional
    /// reservation method.
    StartTimer,
    /// A timer task is already in flight — do not spawn another.
    AlreadyWaiting,
    /// Members returned before the grace period elapsed.
    Cancelled,
    /// The grace period has elapsed — trigger auto-stop.
    Trigger,
}
