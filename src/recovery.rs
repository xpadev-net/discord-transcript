use crate::domain::MeetingStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCandidate {
    pub meeting_id: String,
    pub status: MeetingStatus,
    pub voice_connected: bool,
    pub has_recording_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    Noop,
    ConfirmStopClientDisconnect,
    RequeueSummary,
    MarkFailedMissingRecording,
}

pub fn decide_recovery_action(candidate: &RecoveryCandidate) -> RecoveryAction {
    match candidate.status {
        MeetingStatus::Recording => {
            if candidate.voice_connected {
                RecoveryAction::Noop
            } else if candidate.has_recording_file {
                RecoveryAction::ConfirmStopClientDisconnect
            } else {
                RecoveryAction::MarkFailedMissingRecording
            }
        }
        MeetingStatus::Stopping | MeetingStatus::Transcribing | MeetingStatus::Summarizing => {
            if candidate.has_recording_file {
                // Always (re-)process the summary, whether or not a job was already queued.
                // The runtime's enqueue call is idempotent (AlreadyExists is ignored), so
                // the existing queued job will simply be claimed and run.
                RecoveryAction::RequeueSummary
            } else {
                RecoveryAction::MarkFailedMissingRecording
            }
        }
        MeetingStatus::Scheduled
        | MeetingStatus::Posted
        | MeetingStatus::Failed
        | MeetingStatus::Aborted => RecoveryAction::Noop,
    }
}
