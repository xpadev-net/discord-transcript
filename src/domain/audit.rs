use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub id: String,
    pub tenant_id: Option<String>,
    pub guild_id: Option<String>,
    pub actor_user_id: Option<String>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub request_metadata_json: String,
    pub detail_json: String,
    pub occurred_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct AuditLog {
    events: Vec<AuditEvent>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, event: AuditEvent) {
        self.events.push(event);
    }

    pub fn list(&self) -> &[AuditEvent] {
        &self.events
    }

    pub fn recent(&self, limit: usize) -> Vec<AuditEvent> {
        let mut events = self.events.clone();
        events.sort_by(|left, right| {
            right
                .occurred_at
                .cmp(&left.occurred_at)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| right.id.cmp(&left.id))
        });
        events.truncate(limit);
        events
    }
}
