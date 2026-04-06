use crate::domain::{MeetingStatus, StopReason};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopTransition {
    Acquired,
    AlreadyStoppingOrStopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    AlreadyExists {
        meeting_id: String,
    },
    Backend(String),
    NotFound {
        meeting_id: String,
    },
    /// The meeting exists but its current status does not match the expected
    /// value provided to a CAS-guarded operation.
    CasConflict {
        meeting_id: String,
    },
}

impl Display for StoreError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists { meeting_id } => {
                write!(f, "meeting already exists: {meeting_id}")
            }
            Self::Backend(err) => {
                write!(f, "store backend error: {err}")
            }
            Self::NotFound { meeting_id } => {
                write!(f, "meeting not found: {meeting_id}")
            }
            Self::CasConflict { meeting_id } => {
                write!(
                    f,
                    "meeting status does not match expected value: {meeting_id}"
                )
            }
        }
    }
}

impl std::error::Error for StoreError {}

pub trait MeetingStore {
    fn mark_stopping_if_recording(
        &mut self,
        meeting_id: &str,
        reason: StopReason,
    ) -> Result<StopTransition, StoreError>;

    fn find_active_meeting_by_guild(
        &mut self,
        guild_id: &str,
    ) -> Result<Option<StoredMeeting>, StoreError>;

    fn get_meeting(&mut self, meeting_id: &str) -> Result<Option<StoredMeeting>, StoreError>;

    fn create_scheduled_meeting(&mut self, request: CreateMeetingRequest)
    -> Result<(), StoreError>;

    fn create_meeting_as_recording(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError>;

    /// Update the meeting status. If `expected_current` is provided, the update
    /// is conditional (CAS): only applied when the current status matches.
    /// Returns `StoreError::NotFound` if the meeting does not exist.
    /// Returns `StoreError::CasConflict` if `expected_current` is provided and
    /// the current status does not match the expected value.
    fn set_meeting_status(
        &mut self,
        meeting_id: &str,
        status: MeetingStatus,
        expected_current: Option<MeetingStatus>,
    ) -> Result<(), StoreError>;

    fn set_error_message(
        &mut self,
        meeting_id: &str,
        error_message: Option<String>,
    ) -> Result<(), StoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMeeting {
    pub id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub report_channel_id: String,
    pub started_by_user_id: String,
    pub title: Option<String>,
    pub status: MeetingStatus,
    pub stop_reason: Option<StopReason>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateMeetingRequest {
    pub id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub report_channel_id: String,
    pub started_by_user_id: String,
}

#[derive(Debug, Default)]
pub struct InMemoryMeetingStore {
    meetings: HashMap<String, StoredMeeting>,
}

impl InMemoryMeetingStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, meeting: StoredMeeting) {
        self.meetings.insert(meeting.id.clone(), meeting);
    }

    pub fn get(&self, meeting_id: &str) -> Option<&StoredMeeting> {
        self.meetings.get(meeting_id)
    }

    fn is_active(status: MeetingStatus) -> bool {
        matches!(
            status,
            MeetingStatus::Scheduled | MeetingStatus::Recording | MeetingStatus::Stopping
        )
    }
}

impl MeetingStore for InMemoryMeetingStore {
    fn mark_stopping_if_recording(
        &mut self,
        meeting_id: &str,
        reason: StopReason,
    ) -> Result<StopTransition, StoreError> {
        let Some(meeting) = self.meetings.get_mut(meeting_id) else {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        };

        if meeting.status == MeetingStatus::Recording {
            meeting.status = MeetingStatus::Stopping;
            meeting.stop_reason = Some(reason);
            return Ok(StopTransition::Acquired);
        }

        Ok(StopTransition::AlreadyStoppingOrStopped)
    }

    fn find_active_meeting_by_guild(
        &mut self,
        guild_id: &str,
    ) -> Result<Option<StoredMeeting>, StoreError> {
        Ok(self
            .meetings
            .values()
            .find(|m| m.guild_id == guild_id && Self::is_active(m.status))
            .cloned())
    }

    fn get_meeting(&mut self, meeting_id: &str) -> Result<Option<StoredMeeting>, StoreError> {
        Ok(self.meetings.get(meeting_id).cloned())
    }

    fn create_scheduled_meeting(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError> {
        if self.meetings.contains_key(&request.id) {
            return Err(StoreError::AlreadyExists {
                meeting_id: request.id,
            });
        }

        let meeting = StoredMeeting {
            id: request.id.clone(),
            guild_id: request.guild_id,
            voice_channel_id: request.voice_channel_id,
            report_channel_id: request.report_channel_id,
            started_by_user_id: request.started_by_user_id,
            title: None,
            status: MeetingStatus::Scheduled,
            stop_reason: None,
            error_message: None,
        };
        self.meetings.insert(request.id, meeting);
        Ok(())
    }

    fn create_meeting_as_recording(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError> {
        if self.meetings.contains_key(&request.id) {
            return Err(StoreError::AlreadyExists {
                meeting_id: request.id,
            });
        }

        let meeting = StoredMeeting {
            id: request.id.clone(),
            guild_id: request.guild_id,
            voice_channel_id: request.voice_channel_id,
            report_channel_id: request.report_channel_id,
            started_by_user_id: request.started_by_user_id,
            title: None,
            status: MeetingStatus::Recording,
            stop_reason: None,
            error_message: None,
        };
        self.meetings.insert(request.id, meeting);
        Ok(())
    }

    fn set_meeting_status(
        &mut self,
        meeting_id: &str,
        status: MeetingStatus,
        expected_current: Option<MeetingStatus>,
    ) -> Result<(), StoreError> {
        let Some(meeting) = self.meetings.get_mut(meeting_id) else {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        };
        if let Some(expected) = expected_current
            && meeting.status != expected
        {
            return Err(StoreError::CasConflict {
                meeting_id: meeting_id.to_owned(),
            });
        }
        meeting.status = status;
        Ok(())
    }

    fn set_error_message(
        &mut self,
        meeting_id: &str,
        error_message: Option<String>,
    ) -> Result<(), StoreError> {
        let Some(meeting) = self.meetings.get_mut(meeting_id) else {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        };
        meeting.error_message = error_message;
        Ok(())
    }
}
