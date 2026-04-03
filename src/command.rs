use crate::domain::StopReason;
use crate::stop::{StopMeetingError, StopOutcome, stop_meeting};
use crate::storage::{CreateMeetingRequest, MeetingStore, StoreError};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionSet {
    pub can_connect_voice: bool,
    pub can_send_messages: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordStartRequest {
    pub meeting_id: String,
    pub guild_id: String,
    pub started_by_user_id: String,
    pub command_channel_id: String,
    pub user_voice_channel_id: Option<String>,
    pub permissions: PermissionSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordStartResult {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub report_channel_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordStopRequest {
    pub guild_id: String,
    pub reason: StopReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordStopResult {
    pub meeting_id: String,
    pub outcome: StopOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    UserNotInVoice,
    MissingPermission(&'static str),
    ActiveMeetingExists { meeting_id: String },
    NoActiveMeeting,
    Store(String),
    Stop(String),
}

impl Display for CommandError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserNotInVoice => write!(f, "user is not connected to a voice channel"),
            Self::MissingPermission(kind) => write!(f, "missing required permission: {kind}"),
            Self::ActiveMeetingExists { meeting_id } => {
                write!(f, "an active meeting already exists: {meeting_id}")
            }
            Self::NoActiveMeeting => write!(f, "no active meeting found"),
            Self::Store(err) => write!(f, "{err}"),
            Self::Stop(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CommandError {}

impl From<StoreError> for CommandError {
    fn from(value: StoreError) -> Self {
        Self::Store(value.to_string())
    }
}

impl From<StopMeetingError> for CommandError {
    fn from(value: StopMeetingError) -> Self {
        Self::Stop(value.to_string())
    }
}

pub fn record_start<S: MeetingStore>(
    store: &mut S,
    request: RecordStartRequest,
) -> Result<RecordStartResult, CommandError> {
    let voice_channel_id = request
        .user_voice_channel_id
        .ok_or(CommandError::UserNotInVoice)?;

    if !request.permissions.can_connect_voice {
        return Err(CommandError::MissingPermission("connect_voice"));
    }
    if !request.permissions.can_send_messages {
        return Err(CommandError::MissingPermission("send_messages"));
    }

    if let Some(active) = store.find_active_meeting_by_guild(&request.guild_id)? {
        // Only block new recordings for meetings that are truly active
        // (not yet stopped). Meetings in stopping/transcribing/summarizing
        // should not prevent starting a new recording.
        if matches!(
            active.status,
            crate::domain::MeetingStatus::Scheduled | crate::domain::MeetingStatus::Recording
        ) {
            return Err(CommandError::ActiveMeetingExists {
                meeting_id: active.id,
            });
        }
    }

    store.create_meeting_as_recording(CreateMeetingRequest {
        id: request.meeting_id.clone(),
        guild_id: request.guild_id.clone(),
        voice_channel_id: voice_channel_id.clone(),
        report_channel_id: request.command_channel_id.clone(),
        started_by_user_id: request.started_by_user_id,
    })?;

    Ok(RecordStartResult {
        meeting_id: request.meeting_id,
        guild_id: request.guild_id,
        voice_channel_id,
        report_channel_id: request.command_channel_id,
    })
}

pub fn record_stop<S: MeetingStore>(
    store: &mut S,
    request: RecordStopRequest,
) -> Result<RecordStopResult, CommandError> {
    let meeting_id = store
        .find_active_meeting_by_guild(&request.guild_id)?
        .map(|meeting| meeting.id)
        .ok_or(CommandError::NoActiveMeeting)?;

    let outcome = stop_meeting(store, &meeting_id, request.reason)?;
    Ok(RecordStopResult {
        meeting_id,
        outcome,
    })
}
