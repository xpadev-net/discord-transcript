use crate::domain::usage::{NewUsageEvent, UsageAggregate, UsageEvent};
use crate::domain::{MeetingStatus, StopReason};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopTransition {
    Acquired,
    AlreadyStoppingOrStopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMessageMetadata {
    pub report_channel_id: String,
    pub status_message_channel_id: Option<String>,
    pub status_message_id: Option<String>,
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

pub trait MeetingStore: UsageEventStore {
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

    fn get_status_message_metadata(
        &mut self,
        meeting_id: &str,
    ) -> Result<StatusMessageMetadata, StoreError>;

    fn set_status_message(
        &mut self,
        meeting_id: &str,
        channel_id: String,
        message_id: String,
    ) -> Result<(), StoreError>;

    fn upsert_effective_meeting_settings(
        &mut self,
        meeting_id: &str,
        settings: EffectiveMeetingSettings,
    ) -> Result<(), StoreError>;

    fn get_effective_meeting_settings(
        &mut self,
        meeting_id: &str,
    ) -> Result<Option<EffectiveMeetingSettings>, StoreError>;
}

pub trait UsageEventStore {
    fn append_usage_event(&mut self, event: &NewUsageEvent) -> Result<(), StoreError>;

    /// List recent immutable usage events. Implementations clamp `limit` to at
    /// most 100 rows to keep observe-only reads bounded.
    fn list_recent_usage_events(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<UsageEvent>, StoreError>;

    fn aggregate_recent_usage(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        window_seconds: u64,
    ) -> Result<Vec<UsageAggregate>, StoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMeeting {
    pub id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub report_channel_id: String,
    pub status_message_channel_id: Option<String>,
    pub status_message_id: Option<String>,
    pub started_by_user_id: String,
    pub title: Option<String>,
    pub status: MeetingStatus,
    pub stop_reason: Option<StopReason>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateMeetingRequest {
    pub id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub report_channel_id: String,
    pub status_message_channel_id: Option<String>,
    pub status_message_id: Option<String>,
    pub started_by_user_id: String,
    pub effective_settings: Option<EffectiveMeetingSettings>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeetingSettingsDefaults {
    pub whisper_language: Option<String>,
    pub whisper_vad: bool,
    pub whisper_beam_size: u32,
    pub whisper_suppress_non_speech: bool,
    pub whisper_prompt: Option<String>,
    pub whisper_temperature: f32,
    pub whisper_resample_to_16k: bool,
    pub auto_stop_grace_seconds: u64,
    pub retention_raw_audio_ttl_days: u32,
    pub retention_transcript_ttl_days: u32,
    pub retention_summary_ttl_days: Option<u32>,
    pub summary_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildSettingsForSnapshot {
    pub whisper_language: Option<String>,
    pub whisper_language_explicit: bool,
    pub whisper_vad: Option<bool>,
    pub auto_stop_grace_seconds: Option<u64>,
    pub retention_raw_audio_ttl_days: Option<u32>,
    pub retention_transcript_ttl_days: Option<u32>,
    pub summary_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveMeetingSettings {
    pub whisper_language: Option<String>,
    pub whisper_vad: bool,
    pub whisper_beam_size: u32,
    pub whisper_suppress_non_speech: bool,
    pub whisper_prompt: Option<String>,
    pub whisper_temperature: f32,
    pub whisper_resample_to_16k: bool,
    pub auto_stop_grace_seconds: u64,
    pub retention_raw_audio_ttl_days: u32,
    pub retention_transcript_ttl_days: u32,
    pub retention_summary_ttl_days: Option<u32>,
    pub summary_enabled: bool,
    pub summary_template_id: Option<String>,
    pub domain_knowledge_version_id: Option<String>,
}

impl EffectiveMeetingSettings {
    pub fn from_defaults(defaults: &MeetingSettingsDefaults) -> Self {
        Self {
            whisper_language: defaults.whisper_language.clone(),
            whisper_vad: defaults.whisper_vad,
            whisper_beam_size: defaults.whisper_beam_size,
            whisper_suppress_non_speech: defaults.whisper_suppress_non_speech,
            whisper_prompt: defaults.whisper_prompt.clone(),
            whisper_temperature: defaults.whisper_temperature,
            whisper_resample_to_16k: defaults.whisper_resample_to_16k,
            auto_stop_grace_seconds: defaults.auto_stop_grace_seconds,
            retention_raw_audio_ttl_days: defaults.retention_raw_audio_ttl_days,
            retention_transcript_ttl_days: defaults.retention_transcript_ttl_days,
            retention_summary_ttl_days: defaults.retention_summary_ttl_days,
            summary_enabled: defaults.summary_enabled,
            summary_template_id: None,
            domain_knowledge_version_id: None,
        }
    }

    pub fn resolve(
        defaults: &MeetingSettingsDefaults,
        guild_settings: Option<&GuildSettingsForSnapshot>,
    ) -> Self {
        let mut resolved = Self::from_defaults(defaults);
        let Some(guild_settings) = guild_settings else {
            return resolved;
        };
        if guild_settings.whisper_language_explicit {
            resolved.whisper_language = guild_settings.whisper_language.clone();
        }
        resolved.whisper_vad = guild_settings.whisper_vad.unwrap_or(resolved.whisper_vad);
        resolved.auto_stop_grace_seconds = guild_settings
            .auto_stop_grace_seconds
            .unwrap_or(resolved.auto_stop_grace_seconds);
        resolved.retention_raw_audio_ttl_days = guild_settings
            .retention_raw_audio_ttl_days
            .unwrap_or(resolved.retention_raw_audio_ttl_days);
        resolved.retention_transcript_ttl_days = guild_settings
            .retention_transcript_ttl_days
            .unwrap_or(resolved.retention_transcript_ttl_days);
        resolved.summary_enabled = guild_settings
            .summary_enabled
            .unwrap_or(resolved.summary_enabled);
        resolved
    }
}

#[derive(Debug, Default)]
pub struct InMemoryMeetingStore {
    meetings: HashMap<String, StoredMeeting>,
    effective_settings: HashMap<String, EffectiveMeetingSettings>,
    usage_events: HashMap<String, UsageEvent>,
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

    pub fn get_effective_settings(&self, meeting_id: &str) -> Option<&EffectiveMeetingSettings> {
        self.effective_settings.get(meeting_id)
    }

    fn is_active(status: MeetingStatus) -> bool {
        matches!(
            status,
            MeetingStatus::Scheduled | MeetingStatus::Recording | MeetingStatus::Stopping
        )
    }
}

impl UsageEventStore for InMemoryMeetingStore {
    fn append_usage_event(&mut self, event: &NewUsageEvent) -> Result<(), StoreError> {
        self.usage_events
            .entry(event.id.clone())
            .or_insert_with(|| UsageEvent {
                id: event.id.clone(),
                tenant_id: event.tenant_id.clone(),
                guild_id: event.guild_id.clone(),
                meeting_id: event.meeting_id.clone(),
                job_id: event.job_id.clone(),
                resource_type: event.resource_type.clone(),
                resource_id: event.resource_id.clone(),
                metric: event.metric,
                quantity: event.quantity,
                detail_json: event.detail_json.as_str().to_owned(),
                observed_at: event.observed_at,
                created_at: event.observed_at,
            });
        Ok(())
    }

    fn list_recent_usage_events(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<UsageEvent>, StoreError> {
        let mut events = self
            .usage_events
            .values()
            .filter(|event| in_memory_usage_scope_matches(event, tenant_id, guild_id))
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by(|left, right| {
            right
                .observed_at
                .cmp(&left.observed_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        events.truncate(limit.min(100) as usize);
        Ok(events)
    }

    fn aggregate_recent_usage(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        window_seconds: u64,
    ) -> Result<Vec<UsageAggregate>, StoreError> {
        let now = Utc::now();
        let window_seconds = window_seconds.max(1);
        let mut quantities: HashMap<crate::domain::usage::UsageMetric, i64> = HashMap::new();
        for event in self
            .usage_events
            .values()
            .filter(|event| in_memory_usage_scope_matches(event, tenant_id, guild_id))
            .filter(|event| {
                now.signed_duration_since(event.observed_at)
                    .num_seconds()
                    .max(0) as u64
                    <= window_seconds
            })
        {
            *quantities.entry(event.metric).or_default() += event.quantity;
        }
        Ok(quantities
            .into_iter()
            .map(|(metric, quantity)| UsageAggregate { metric, quantity })
            .collect())
    }
}

fn in_memory_usage_scope_matches(
    event: &UsageEvent,
    tenant_id: Option<&str>,
    guild_id: Option<&str>,
) -> bool {
    if guild_id.is_some_and(|guild_id| event.guild_id != guild_id) {
        return false;
    }
    let Some(tenant_id) = tenant_id else {
        return true;
    };
    if event.tenant_id.as_deref() == Some(tenant_id) {
        return true;
    }
    event.tenant_id.is_none() && guild_id.is_some()
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
            meeting.stopped_at = Some(Utc::now());
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

        let meeting_id = request.id.clone();
        let effective_settings = request.effective_settings.clone();
        let meeting = StoredMeeting {
            id: request.id.clone(),
            guild_id: request.guild_id,
            voice_channel_id: request.voice_channel_id,
            report_channel_id: request.report_channel_id,
            status_message_channel_id: request.status_message_channel_id,
            status_message_id: request.status_message_id,
            started_by_user_id: request.started_by_user_id,
            title: None,
            status: MeetingStatus::Scheduled,
            stop_reason: None,
            error_message: None,
            started_at: None,
            stopped_at: None,
        };
        self.meetings.insert(request.id, meeting);
        if let Some(settings) = effective_settings {
            self.effective_settings.insert(meeting_id, settings);
        }
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

        let meeting_id = request.id.clone();
        let effective_settings = request.effective_settings.clone();
        let meeting = StoredMeeting {
            id: request.id.clone(),
            guild_id: request.guild_id,
            voice_channel_id: request.voice_channel_id,
            report_channel_id: request.report_channel_id,
            status_message_channel_id: request.status_message_channel_id,
            status_message_id: request.status_message_id,
            started_by_user_id: request.started_by_user_id,
            title: None,
            status: MeetingStatus::Recording,
            stop_reason: None,
            error_message: None,
            started_at: Some(Utc::now()),
            stopped_at: None,
        };
        self.meetings.insert(request.id, meeting);
        if let Some(settings) = effective_settings {
            self.effective_settings.insert(meeting_id, settings);
        }
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

    fn get_status_message_metadata(
        &mut self,
        meeting_id: &str,
    ) -> Result<StatusMessageMetadata, StoreError> {
        let Some(meeting) = self.meetings.get(meeting_id) else {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        };

        Ok(StatusMessageMetadata {
            report_channel_id: meeting.report_channel_id.clone(),
            status_message_channel_id: meeting.status_message_channel_id.clone(),
            status_message_id: meeting.status_message_id.clone(),
        })
    }

    fn set_status_message(
        &mut self,
        meeting_id: &str,
        channel_id: String,
        message_id: String,
    ) -> Result<(), StoreError> {
        let Some(meeting) = self.meetings.get_mut(meeting_id) else {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        };
        meeting.status_message_channel_id = Some(channel_id);
        meeting.status_message_id = Some(message_id);
        Ok(())
    }

    fn upsert_effective_meeting_settings(
        &mut self,
        meeting_id: &str,
        settings: EffectiveMeetingSettings,
    ) -> Result<(), StoreError> {
        if !self.meetings.contains_key(meeting_id) {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        }
        self.effective_settings
            .insert(meeting_id.to_owned(), settings);
        Ok(())
    }

    fn get_effective_meeting_settings(
        &mut self,
        meeting_id: &str,
    ) -> Result<Option<EffectiveMeetingSettings>, StoreError> {
        Ok(self.effective_settings.get(meeting_id).cloned())
    }
}
