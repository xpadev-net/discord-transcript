use chrono::{DateTime, Utc};

use super::confidence::ConfidencePermille;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonAliasSourceType {
    UserFeedback,
    AiInference,
    VcParticipant,
    Manual,
}

impl PersonAliasSourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserFeedback => "user_feedback",
            Self::AiInference => "ai_inference",
            Self::VcParticipant => "vc_participant",
            Self::Manual => "manual",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "user_feedback" => Some(Self::UserFeedback),
            "ai_inference" => Some(Self::AiInference),
            "vc_participant" => Some(Self::VcParticipant),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonAliasReviewStatus {
    Unreviewed,
    Accepted,
    Dismissed,
}

impl PersonAliasReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreviewed => "unreviewed",
            Self::Accepted => "accepted",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "unreviewed" => Some(Self::Unreviewed),
            "accepted" => Some(Self::Accepted),
            "dismissed" => Some(Self::Dismissed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonAlias {
    pub id: String,
    pub tenant_discord_guild_id: String,
    pub tenant_id: String,
    pub guild_id: String,
    pub canonical_name: String,
    pub alias: String,
    pub discord_user_id: Option<String>,
    pub source_type: PersonAliasSourceType,
    pub source_meeting_id: Option<String>,
    pub source_feedback_id: Option<String>,
    pub confidence: Option<ConfidencePermille>,
    pub active: bool,
    pub review_status: PersonAliasReviewStatus,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_actor_user_id: Option<String>,
    pub archived_at: Option<DateTime<Utc>>,
    pub archived_actor_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
