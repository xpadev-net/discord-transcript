use crate::application::stop::{StopMeetingError, StopOutcome, stop_meeting};
use crate::domain::StopReason;
use crate::domain::authz::{Action, UserRole, is_allowed};
use crate::infrastructure::storage::{
    CreateMeetingRequest, EffectiveMeetingSettings, MeetingStore, StoreError, StoredMeeting,
};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionSet {
    pub can_connect_voice: bool,
    pub can_send_messages: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordStartRequest {
    pub meeting_id: String,
    pub guild_id: String,
    pub started_by_user_id: String,
    pub command_channel_id: String,
    pub user_voice_channel_id: Option<String>,
    pub permissions: PermissionSet,
    pub caller_role: UserRole,
    pub effective_settings: Option<EffectiveMeetingSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordStartResult {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub report_channel_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordStartPreflight {
    voice_channel_id: String,
}

impl RecordStartPreflight {
    pub(crate) fn voice_channel_id(&self) -> &str {
        &self.voice_channel_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordStopRequest {
    pub guild_id: String,
    pub caller_user_id: String,
    pub caller_role: UserRole,
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
    Unauthorized(&'static str),
    ActiveMeetingExists {
        meeting_id: String,
    },
    /// A meeting with the given ID already exists in the store (duplicate key).
    AlreadyExists {
        meeting_id: String,
    },
    PreflightMismatch,
    NoActiveMeeting,
    Store(String),
    Stop(String),
}

impl Display for CommandError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserNotInVoice => write!(f, "user is not connected to a voice channel"),
            Self::MissingPermission(kind) => write!(f, "missing required permission: {kind}"),
            Self::Unauthorized(action) => write!(f, "not authorized to {action}"),
            Self::ActiveMeetingExists { meeting_id } => {
                write!(f, "an active meeting already exists: {meeting_id}")
            }
            Self::AlreadyExists { meeting_id } => {
                write!(f, "meeting already exists: {meeting_id}")
            }
            Self::PreflightMismatch => write!(f, "record-start preflight does not match request"),
            Self::NoActiveMeeting => write!(f, "no active meeting found"),
            Self::Store(err) => write!(f, "{err}"),
            Self::Stop(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CommandError {}

impl From<StoreError> for CommandError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::AlreadyExists { meeting_id } => Self::AlreadyExists { meeting_id },
            other => Self::Store(other.to_string()),
        }
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
    let preflight = validate_record_start_preconditions(store, &request)?;
    record_start_after_preflight(store, request, preflight)
}

pub(crate) fn record_start_after_preflight<S: MeetingStore>(
    store: &mut S,
    request: RecordStartRequest,
    preflight: RecordStartPreflight,
) -> Result<RecordStartResult, CommandError> {
    // Production derives both values from the same resolved voice channel. This
    // protects direct callers, tests, and future call sites from mixing a
    // request with a preflight token created for a different channel.
    let Some(request_voice_channel_id) = request.user_voice_channel_id.as_deref() else {
        return Err(CommandError::UserNotInVoice);
    };
    if request_voice_channel_id != preflight.voice_channel_id() {
        return Err(CommandError::PreflightMismatch);
    }
    let voice_channel_id = preflight.voice_channel_id().to_owned();
    store.create_meeting_as_recording(CreateMeetingRequest {
        id: request.meeting_id.clone(),
        guild_id: request.guild_id.clone(),
        voice_channel_id: voice_channel_id.clone(),
        report_channel_id: request.command_channel_id.clone(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: request.started_by_user_id,
        effective_settings: request.effective_settings,
    })?;

    Ok(RecordStartResult {
        meeting_id: request.meeting_id,
        guild_id: request.guild_id,
        voice_channel_id,
        report_channel_id: request.command_channel_id,
    })
}

pub(crate) fn validate_record_start_preconditions<S: MeetingStore>(
    store: &mut S,
    request: &RecordStartRequest,
) -> Result<RecordStartPreflight, CommandError> {
    // The production handler checks this before building the request; keep the
    // guard here for direct tests and future callers of the preflight helper.
    let voice_channel_id = request
        .user_voice_channel_id
        .clone()
        .ok_or(CommandError::UserNotInVoice)?;

    if !request.permissions.can_connect_voice {
        return Err(CommandError::MissingPermission("connect_voice"));
    }
    if !request.permissions.can_send_messages {
        return Err(CommandError::MissingPermission("send_messages"));
    }
    if !is_allowed(request.caller_role, Action::StartRecording) {
        return Err(CommandError::Unauthorized("start recording"));
    }

    if let Some(active) = store.find_active_meeting_by_guild(&request.guild_id)? {
        // find_active_meeting_by_guild returns Scheduled/Recording/Stopping.
        // Only block new recordings for Scheduled/Recording — a Stopping
        // meeting has already released the voice channel and should not
        // prevent starting a new recording.
        if matches!(
            active.status,
            crate::domain::MeetingStatus::Scheduled | crate::domain::MeetingStatus::Recording
        ) {
            return Err(CommandError::ActiveMeetingExists {
                meeting_id: active.id,
            });
        }
    }

    Ok(RecordStartPreflight { voice_channel_id })
}

pub fn record_stop<S: MeetingStore>(
    store: &mut S,
    request: RecordStopRequest,
) -> Result<RecordStopResult, CommandError> {
    let meeting = store
        .find_active_meeting_by_guild(&request.guild_id)?
        .ok_or(CommandError::NoActiveMeeting)?;
    authorize_record_stop_for_meeting(&meeting, &request.caller_user_id, request.caller_role)?;

    // A Scheduled meeting was never actually recording — abort it directly
    // rather than sending it through the stop_meeting CAS path which only
    // handles the Recording→Stopping transition.
    if meeting.status == crate::domain::MeetingStatus::Scheduled {
        store.set_meeting_status(
            &meeting.id,
            crate::domain::MeetingStatus::Aborted,
            Some(crate::domain::MeetingStatus::Scheduled),
        )?;
        return Ok(RecordStopResult {
            meeting_id: meeting.id,
            outcome: StopOutcome::AlreadyHandled,
        });
    }

    let outcome = stop_meeting(store, &meeting.id, request.reason)?;
    Ok(RecordStopResult {
        meeting_id: meeting.id,
        outcome,
    })
}

pub fn authorize_record_stop_for_meeting(
    meeting: &StoredMeeting,
    caller_user_id: &str,
    caller_role: UserRole,
) -> Result<(), CommandError> {
    let effective_role = match caller_role {
        UserRole::BotAdmin | UserRole::GuildAdmin => caller_role,
        UserRole::Member | UserRole::StartedMeeting => {
            if meeting.started_by_user_id == caller_user_id {
                UserRole::StartedMeeting
            } else {
                UserRole::Member
            }
        }
    };
    if is_allowed(effective_role, Action::StopRecording) {
        Ok(())
    } else {
        Err(CommandError::Unauthorized("stop recording"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::storage::InMemoryMeetingStore;

    fn default_start_request() -> RecordStartRequest {
        RecordStartRequest {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            started_by_user_id: "u1".to_owned(),
            command_channel_id: "report-chan".to_owned(),
            user_voice_channel_id: Some("vc-1".to_owned()),
            permissions: PermissionSet {
                can_connect_voice: true,
                can_send_messages: true,
            },
            caller_role: UserRole::GuildAdmin,
            effective_settings: None,
        }
    }

    #[test]
    fn record_start_after_preflight_rejects_mismatched_voice_channel() {
        let mut store = InMemoryMeetingStore::new();
        let request = default_start_request();
        let preflight = RecordStartPreflight {
            voice_channel_id: "vc-2".to_owned(),
        };

        let error = record_start_after_preflight(&mut store, request, preflight)
            .expect_err("mismatched preflight should be rejected");

        assert_eq!(error, CommandError::PreflightMismatch);
        assert!(
            store.get("m1").is_none(),
            "mismatched preflight must not create a meeting"
        );
    }

    #[test]
    fn record_start_after_preflight_preserves_missing_voice_error() {
        let mut store = InMemoryMeetingStore::new();
        let mut request = default_start_request();
        request.user_voice_channel_id = None;
        let preflight = RecordStartPreflight {
            voice_channel_id: "vc-1".to_owned(),
        };

        let error = record_start_after_preflight(&mut store, request, preflight)
            .expect_err("missing voice channel should use the preflight user error");

        assert_eq!(error, CommandError::UserNotInVoice);
        assert!(
            store.get("m1").is_none(),
            "missing voice channel must not create a meeting"
        );
    }
}
