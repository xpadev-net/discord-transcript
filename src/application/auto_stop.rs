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
    #[cfg(test)]
    pub(crate) fn new(grace_period: Duration) -> Self {
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

    /// Returns true only for a state that is explicitly scoped to `meeting_id`.
    /// An unscoped state is legacy/unknown local state, not a wildcard match;
    /// cleanup paths use `should_remove_for_meeting_cleanup` for that case.
    pub fn belongs_to_meeting(&self, meeting_id: &str) -> bool {
        match self.meeting_id.as_deref() {
            None => false,
            Some(current) => current == meeting_id,
        }
    }

    pub fn should_remove_for_meeting_cleanup(&self, meeting_id: &str) -> bool {
        match self.meeting_id.as_deref() {
            None => true,
            Some(current) => current == meeting_id,
        }
    }

    pub fn refresh_for_meeting(&mut self, grace_period: Duration, meeting_id: &str) -> bool {
        if self.meeting_id.as_deref() == Some(meeting_id) {
            return false;
        }

        let previous_generation = self.timer_generation;
        *self = Self::new_for_meeting(grace_period, Some(meeting_id.to_owned()));
        self.timer_generation = previous_generation;
        true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_meeting_state_is_scoped() {
        let state = AutoStopState::new_for_meeting(Duration::from_secs(30), Some("m1".to_owned()));

        assert_eq!(state.meeting_id(), Some("m1"));
        assert!(state.belongs_to_meeting("m1"));
        assert!(!state.belongs_to_meeting("m2"));
    }

    #[test]
    fn unscoped_state_refreshes_to_known_meeting_before_timer_reuse() {
        let mut state = AutoStopState::new(Duration::from_secs(30));
        assert_eq!(
            state.on_non_bot_member_count_changed(0),
            AutoStopSignal::StartTimer
        );

        assert!(state.refresh_for_meeting(Duration::from_secs(60), "m1"));
        assert_eq!(state.meeting_id(), Some("m1"));
        assert!(state.belongs_to_meeting("m1"));
        let unscoped = AutoStopState::new(Duration::from_secs(60));
        assert!(!unscoped.belongs_to_meeting("m1"));
        assert!(unscoped.should_remove_for_meeting_cleanup("m1"));
        assert_eq!(state.timer_generation(), 1);
        assert_eq!(
            state.on_non_bot_member_count_changed(0),
            AutoStopSignal::StartTimer
        );
        assert_eq!(state.timer_generation(), 2);
    }

    #[test]
    fn refresh_for_meeting_preserves_generation_to_avoid_stale_timer_collision() {
        let mut state = AutoStopState::new(Duration::from_secs(30));
        assert_eq!(
            state.on_non_bot_member_count_changed(0),
            AutoStopSignal::StartTimer
        );
        let stale_generation = state.timer_generation();

        assert!(state.refresh_for_meeting(Duration::from_secs(60), "m1"));
        assert_eq!(
            state.on_non_bot_member_count_changed(0),
            AutoStopSignal::StartTimer
        );
        assert_ne!(state.timer_generation(), stale_generation);

        state.clear_timer_active_for_generation(stale_generation);
        assert_eq!(
            state.on_non_bot_member_count_changed(0),
            AutoStopSignal::AlreadyWaiting,
            "a stale timer must not clear the replacement timer reservation"
        );
    }
}
