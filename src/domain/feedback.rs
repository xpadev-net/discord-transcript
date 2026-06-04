use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFeedbackType {
    Mistranscription,
    Speaker,
    Term,
    PersonAlias,
    DomainKnowledge,
    AiMemory,
}

impl TranscriptFeedbackType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mistranscription => "mistranscription",
            Self::Speaker => "speaker",
            Self::Term => "term",
            Self::PersonAlias => "person_alias",
            Self::DomainKnowledge => "domain_knowledge",
            Self::AiMemory => "ai_memory",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "mistranscription" => Some(Self::Mistranscription),
            "speaker" => Some(Self::Speaker),
            "term" => Some(Self::Term),
            "person_alias" => Some(Self::PersonAlias),
            "domain_knowledge" => Some(Self::DomainKnowledge),
            "ai_memory" => Some(Self::AiMemory),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFeedbackTermType {
    GeneralTerm,
    PersonName,
    ProjectName,
    ProductName,
    Organization,
    Acronym,
    WordingRule,
    ProhibitedItem,
}

impl TranscriptFeedbackTermType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GeneralTerm => "general_term",
            Self::PersonName => "person_name",
            Self::ProjectName => "project_name",
            Self::ProductName => "product_name",
            Self::Organization => "organization",
            Self::Acronym => "acronym",
            Self::WordingRule => "wording_rule",
            Self::ProhibitedItem => "prohibited_item",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "general_term" => Some(Self::GeneralTerm),
            "person_name" => Some(Self::PersonName),
            "project_name" => Some(Self::ProjectName),
            "product_name" => Some(Self::ProductName),
            "organization" => Some(Self::Organization),
            "acronym" => Some(Self::Acronym),
            "wording_rule" => Some(Self::WordingRule),
            "prohibited_item" => Some(Self::ProhibitedItem),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFeedbackStatus {
    Open,
    Accepted,
    Dismissed,
    ConvertedToDomainKnowledge,
    ConvertedToAiMemory,
}

impl TranscriptFeedbackStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Accepted => "accepted",
            Self::Dismissed => "dismissed",
            Self::ConvertedToDomainKnowledge => "converted_to_domain_knowledge",
            Self::ConvertedToAiMemory => "converted_to_ai_memory",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "accepted" => Some(Self::Accepted),
            "dismissed" => Some(Self::Dismissed),
            "converted_to_domain_knowledge" => Some(Self::ConvertedToDomainKnowledge),
            "converted_to_ai_memory" => Some(Self::ConvertedToAiMemory),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFeedback {
    pub id: String,
    pub tenant_discord_guild_id: String,
    pub tenant_id: String,
    pub guild_id: String,
    pub meeting_id: Option<String>,
    pub transcript_segment_id: Option<String>,
    pub feedback_type: TranscriptFeedbackType,
    pub term_type: Option<TranscriptFeedbackTermType>,
    pub original_text: Option<String>,
    pub corrected_text: Option<String>,
    pub speaker_id: Option<String>,
    pub corrected_speaker_id: Option<String>,
    pub note: Option<String>,
    pub target_domain_knowledge_id: Option<String>,
    pub target_ai_memory_note_id: Option<String>,
    pub actor_user_id: String,
    pub status: TranscriptFeedbackStatus,
    pub created_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_actor_user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTranscriptFeedback {
    pub id: String,
    pub tenant_discord_guild_id: String,
    pub tenant_id: String,
    pub guild_id: String,
    pub meeting_id: Option<String>,
    pub transcript_segment_id: Option<String>,
    pub feedback_type: TranscriptFeedbackType,
    pub term_type: Option<TranscriptFeedbackTermType>,
    pub original_text: Option<String>,
    pub corrected_text: Option<String>,
    pub speaker_id: Option<String>,
    pub corrected_speaker_id: Option<String>,
    pub note: Option<String>,
    pub target_domain_knowledge_id: Option<String>,
    pub target_ai_memory_note_id: Option<String>,
    pub actor_user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateTranscriptFeedbackStatus {
    pub id: String,
    pub tenant_id: String,
    pub guild_id: String,
    pub status: TranscriptFeedbackStatus,
    pub target_domain_knowledge_id: Option<String>,
    pub target_ai_memory_note_id: Option<String>,
    pub reviewed_actor_user_id: String,
}
