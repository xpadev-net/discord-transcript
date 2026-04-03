use crate::domain::StopReason;
use crate::recovery::{RecoveryAction, RecoveryCandidate, decide_recovery_action};
use crate::stop::{StopMeetingError, stop_meeting};
use crate::domain::MeetingStatus;
use crate::storage::{MeetingStore, StoreError};
use std::fmt::{Display, Formatter};
use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryEffect {
    Noop { meeting_id: String },
    StopConfirmedClientDisconnect { meeting_id: String },
    SummaryRequeued { meeting_id: String },
    MarkedFailed { meeting_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryRunnerError {
    Store(String),
    Stop(String),
}

impl Display for RecoveryRunnerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(err) => write!(f, "store error: {err}"),
            Self::Stop(err) => write!(f, "stop error: {err}"),
        }
    }
}

impl std::error::Error for RecoveryRunnerError {}

impl From<StoreError> for RecoveryRunnerError {
    fn from(value: StoreError) -> Self {
        Self::Store(value.to_string())
    }
}

impl From<StopMeetingError> for RecoveryRunnerError {
    fn from(value: StopMeetingError) -> Self {
        Self::Stop(value.to_string())
    }
}

pub fn run_recovery<S: MeetingStore>(
    store: &mut S,
    candidate: &RecoveryCandidate,
) -> Result<RecoveryEffect, RecoveryRunnerError> {
    match decide_recovery_action(candidate) {
        RecoveryAction::Noop => {
            info!(
                meeting_id = %candidate.meeting_id,
                status = %candidate.status.as_str(),
                "recovery noop"
            );
            Ok(RecoveryEffect::Noop {
                meeting_id: candidate.meeting_id.clone(),
            })
        }
        RecoveryAction::ConfirmStopClientDisconnect => {
            let _ = stop_meeting(store, &candidate.meeting_id, StopReason::ClientDisconnect)?;
            info!(
                meeting_id = %candidate.meeting_id,
                "recovery confirmed stop with client_disconnect"
            );
            Ok(RecoveryEffect::StopConfirmedClientDisconnect {
                meeting_id: candidate.meeting_id.clone(),
            })
        }
        RecoveryAction::RequeueSummary => {
            // If the meeting was mid-pipeline (Transcribing/Summarizing), reset
            // to Stopping so process_enqueued_summary_job can drive it forward
            // from a consistent state.
            if candidate.status == MeetingStatus::Transcribing
                || candidate.status == MeetingStatus::Summarizing
            {
                store.set_meeting_status(&candidate.meeting_id, MeetingStatus::Stopping)?;
            }
            info!(
                meeting_id = %candidate.meeting_id,
                "recovery signaled summary pipeline requeue"
            );
            Ok(RecoveryEffect::SummaryRequeued {
                meeting_id: candidate.meeting_id.clone(),
            })
        }
        RecoveryAction::MarkFailedMissingRecording => {
            store.set_meeting_status(&candidate.meeting_id, MeetingStatus::Failed)?;
            store.set_error_message(
                &candidate.meeting_id,
                Some("missing recording artifact during recovery".to_owned()),
            )?;
            warn!(
                meeting_id = %candidate.meeting_id,
                "recovery marked failed due to missing recording"
            );
            Ok(RecoveryEffect::MarkedFailed {
                meeting_id: candidate.meeting_id.clone(),
            })
        }
    }
}
