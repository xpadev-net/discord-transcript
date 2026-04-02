#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserRole {
    StartedMeeting,
    GuildAdmin,
    BotAdmin,
    Member,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    View,
    Reprocess,
    Delete,
}

pub fn is_allowed(role: UserRole, action: Action) -> bool {
    match role {
        UserRole::BotAdmin => true,
        UserRole::GuildAdmin => true,
        UserRole::StartedMeeting => matches!(action, Action::View | Action::Delete),
        UserRole::Member => matches!(action, Action::View),
    }
}
