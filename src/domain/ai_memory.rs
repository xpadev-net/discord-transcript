use chrono::{DateTime, Utc};

use super::confidence::ConfidencePermille;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiMemorySourceType {
    AiMeetingExtraction,
    UserFeedback,
    Manual,
    VcParticipant,
    PromotionCandidate,
}

impl AiMemorySourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AiMeetingExtraction => "ai_meeting_extraction",
            Self::UserFeedback => "user_feedback",
            Self::Manual => "manual",
            Self::VcParticipant => "vc_participant",
            Self::PromotionCandidate => "promotion_candidate",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "ai_meeting_extraction" => Some(Self::AiMeetingExtraction),
            "user_feedback" => Some(Self::UserFeedback),
            "manual" => Some(Self::Manual),
            "vc_participant" => Some(Self::VcParticipant),
            "promotion_candidate" => Some(Self::PromotionCandidate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiMemoryTag {
    Person,
    Alias,
    Project,
    Product,
    Terminology,
    Decision,
    TeamConvention,
    SummaryHint,
    TranscriptionHint,
    Uncertain,
}

impl AiMemoryTag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Alias => "alias",
            Self::Project => "project",
            Self::Product => "product",
            Self::Terminology => "terminology",
            Self::Decision => "decision",
            Self::TeamConvention => "team_convention",
            Self::SummaryHint => "summary_hint",
            Self::TranscriptionHint => "transcription_hint",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "person" => Some(Self::Person),
            "alias" => Some(Self::Alias),
            "project" => Some(Self::Project),
            "product" => Some(Self::Product),
            "terminology" => Some(Self::Terminology),
            "decision" => Some(Self::Decision),
            "team_convention" => Some(Self::TeamConvention),
            "summary_hint" => Some(Self::SummaryHint),
            "transcription_hint" => Some(Self::TranscriptionHint),
            "uncertain" => Some(Self::Uncertain),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiMemoryNote {
    pub id: String,
    pub tenant_discord_guild_id: String,
    pub tenant_id: String,
    pub guild_id: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<AiMemoryTag>,
    pub source_type: AiMemorySourceType,
    pub source_meeting_id: Option<String>,
    pub source_feedback_id: Option<String>,
    pub confidence: Option<ConfidencePermille>,
    pub active: bool,
    pub pinned: bool,
    pub created_actor_user_id: String,
    pub updated_actor_user_id: String,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub archived_actor_user_id: Option<String>,
}
