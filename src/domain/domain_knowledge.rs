use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainKnowledgeContentType {
    Glossary,
    PersonName,
    ProjectContext,
    WordingRule,
    ProhibitedItem,
}

impl DomainKnowledgeContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Glossary => "glossary",
            Self::PersonName => "person_name",
            Self::ProjectContext => "project_context",
            Self::WordingRule => "wording_rule",
            Self::ProhibitedItem => "prohibited_item",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "glossary" => Some(Self::Glossary),
            "person_name" => Some(Self::PersonName),
            "project_context" => Some(Self::ProjectContext),
            "wording_rule" => Some(Self::WordingRule),
            "prohibited_item" => Some(Self::ProhibitedItem),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainKnowledgeItem {
    pub id: String,
    pub tenant_id: Option<String>,
    pub guild_id: String,
    pub content_type: DomainKnowledgeContentType,
    pub title: String,
    pub body: String,
    pub active: bool,
    pub version: u32,
    pub updated_actor_user_id: Option<String>,
    pub archived_at: Option<DateTime<Utc>>,
    pub archived_actor_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDomainKnowledgeItem {
    pub id: String,
    pub guild_id: String,
    pub content_type: DomainKnowledgeContentType,
    pub title: String,
    pub body: String,
    pub active: bool,
    pub updated_actor_user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateDomainKnowledgeItem {
    pub id: String,
    pub guild_id: String,
    pub content_type: DomainKnowledgeContentType,
    pub title: String,
    pub body: String,
    pub active: bool,
    pub updated_actor_user_id: Option<String>,
}
