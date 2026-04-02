use crate::domain::MeetingStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCandidate {
    pub meeting_id: String,
    pub status: MeetingStatus,
    pub voice_connected: bool,
    pub has_recording_file: bool,
    pub summary_job_already_queued: bool,
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
        MeetingStatus::Stopping => {
            if candidate.summary_job_already_queued {
                RecoveryAction::Noop
            } else if candidate.has_recording_file {
                RecoveryAction::RequeueSummary
            } else {
                RecoveryAction::MarkFailedMissingRecording
            }
        }
        _ => RecoveryAction::Noop,
    }
}
