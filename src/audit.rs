#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub actor_user_id: String,
    pub action: String,
    pub meeting_id: String,
    pub detail: String,
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
}
