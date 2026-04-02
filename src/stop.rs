use crate::domain::StopReason;
use crate::storage::{MeetingStore, StopTransition, StoreError};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Owner,
    AlreadyHandled,
}

#[derive(Debug)]
pub enum StopMeetingError {
    Store(StoreError),
}

impl Display for StopMeetingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for StopMeetingError {}

impl From<StoreError> for StopMeetingError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

pub fn stop_meeting<S: MeetingStore>(
    store: &mut S,
    meeting_id: &str,
    reason: StopReason,
) -> Result<StopOutcome, StopMeetingError> {
    let transition = store.mark_stopping_if_recording(meeting_id, reason)?;
    match transition {
        StopTransition::Acquired => Ok(StopOutcome::Owner),
        StopTransition::AlreadyStoppingOrStopped => Ok(StopOutcome::AlreadyHandled),
    }
}
