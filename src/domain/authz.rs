use std::collections::HashSet;
use std::str::FromStr;

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
    StartRecording,
    StopRecording,
}

pub fn is_allowed(role: UserRole, action: Action) -> bool {
    match role {
        UserRole::BotAdmin => true,
        UserRole::GuildAdmin => true,
        UserRole::StartedMeeting => {
            matches!(
                action,
                Action::View | Action::Delete | Action::StopRecording
            )
        }
        UserRole::Member => matches!(action, Action::View),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RbacPermission {
    RecordingStart,
    RecordingStop,
    MeetingView,
    MeetingReprocess,
    MeetingDelete,
    SettingsManage,
    SummaryTemplateManage,
    DomainKnowledgeManage,
    UsageView,
    AdminView,
}

impl RbacPermission {
    pub const ALL: [Self; 10] = [
        Self::RecordingStart,
        Self::RecordingStop,
        Self::MeetingView,
        Self::MeetingReprocess,
        Self::MeetingDelete,
        Self::SettingsManage,
        Self::SummaryTemplateManage,
        Self::DomainKnowledgeManage,
        Self::UsageView,
        Self::AdminView,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordingStart => "recording:start",
            Self::RecordingStop => "recording:stop",
            Self::MeetingView => "meeting:view",
            Self::MeetingReprocess => "meeting:reprocess",
            Self::MeetingDelete => "meeting:delete",
            Self::SettingsManage => "settings:manage",
            Self::SummaryTemplateManage => "summary_template:manage",
            Self::DomainKnowledgeManage => "domain_knowledge:manage",
            Self::UsageView => "usage:view",
            Self::AdminView => "admin:view",
        }
    }
}

impl FromStr for RbacPermission {
    type Err = RbacPermissionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "recording:start" => Ok(Self::RecordingStart),
            "recording:stop" => Ok(Self::RecordingStop),
            "meeting:view" => Ok(Self::MeetingView),
            "meeting:reprocess" => Ok(Self::MeetingReprocess),
            "meeting:delete" => Ok(Self::MeetingDelete),
            "settings:manage" => Ok(Self::SettingsManage),
            "summary_template:manage" => Ok(Self::SummaryTemplateManage),
            "domain_knowledge:manage" => Ok(Self::DomainKnowledgeManage),
            "usage:view" => Ok(Self::UsageView),
            "admin:view" => Ok(Self::AdminView),
            _ => Err(RbacPermissionParseError),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RbacPermissionParseError;

impl From<Action> for RbacPermission {
    fn from(action: Action) -> Self {
        match action {
            Action::View => Self::MeetingView,
            Action::Reprocess => Self::MeetingReprocess,
            Action::Delete => Self::MeetingDelete,
            Action::StartRecording => Self::RecordingStart,
            Action::StopRecording => Self::RecordingStop,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RbacRoleGrant {
    pub guild_id: String,
    pub discord_role_id: String,
    pub permission: RbacPermission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRoleSource<'a> {
    Available(&'a [String]),
    LookupFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RbacSubject {
    pub is_bot_admin: bool,
    pub is_guild_admin: bool,
    pub has_channel_view: bool,
    pub is_meeting_starter: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecisionSource {
    BotAdmin,
    GuildAdmin,
    LegacyChannelView,
    LegacyMeetingStarter,
    RbacRole { discord_role_id: String },
    NoGrant,
    MemberRoleLookupFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    pub permission: RbacPermission,
    pub allowed: bool,
    pub source: PermissionDecisionSource,
}

impl PermissionDecision {
    fn allow(permission: RbacPermission, source: PermissionDecisionSource) -> Self {
        Self {
            permission,
            allowed: true,
            source,
        }
    }

    fn deny(permission: RbacPermission, source: PermissionDecisionSource) -> Self {
        Self {
            permission,
            allowed: false,
            source,
        }
    }
}

pub fn resolve_action_permission(
    action: Action,
    guild_id: &str,
    subject: RbacSubject,
    member_roles: MemberRoleSource<'_>,
    role_grants: &[RbacRoleGrant],
) -> PermissionDecision {
    resolve_rbac_permission(
        RbacPermission::from(action),
        guild_id,
        subject,
        member_roles,
        role_grants,
    )
}

pub fn resolve_rbac_permission(
    permission: RbacPermission,
    guild_id: &str,
    subject: RbacSubject,
    member_roles: MemberRoleSource<'_>,
    role_grants: &[RbacRoleGrant],
) -> PermissionDecision {
    if subject.is_bot_admin {
        return PermissionDecision::allow(permission, PermissionDecisionSource::BotAdmin);
    }
    if subject.is_guild_admin {
        return PermissionDecision::allow(permission, PermissionDecisionSource::GuildAdmin);
    }
    if legacy_access_allows(subject, permission) {
        return PermissionDecision::allow(permission, legacy_access_source(subject, permission));
    }

    let MemberRoleSource::Available(member_roles) = member_roles else {
        return PermissionDecision::deny(
            permission,
            PermissionDecisionSource::MemberRoleLookupFailed,
        );
    };
    let member_roles = member_roles
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if let Some(grant) = role_grants.iter().find(|grant| {
        grant.guild_id == guild_id
            && grant.permission == permission
            && member_roles.contains(grant.discord_role_id.as_str())
    }) {
        return PermissionDecision::allow(
            permission,
            PermissionDecisionSource::RbacRole {
                discord_role_id: grant.discord_role_id.clone(),
            },
        );
    }

    PermissionDecision::deny(permission, PermissionDecisionSource::NoGrant)
}

fn legacy_access_allows(subject: RbacSubject, permission: RbacPermission) -> bool {
    match permission {
        RbacPermission::MeetingView => subject.has_channel_view || subject.is_meeting_starter,
        RbacPermission::MeetingDelete | RbacPermission::RecordingStop => subject.is_meeting_starter,
        RbacPermission::RecordingStart
        | RbacPermission::MeetingReprocess
        | RbacPermission::SettingsManage
        | RbacPermission::SummaryTemplateManage
        | RbacPermission::DomainKnowledgeManage
        | RbacPermission::UsageView
        | RbacPermission::AdminView => false,
    }
}

fn legacy_access_source(
    subject: RbacSubject,
    permission: RbacPermission,
) -> PermissionDecisionSource {
    if subject.is_meeting_starter
        && matches!(
            permission,
            RbacPermission::MeetingView
                | RbacPermission::MeetingDelete
                | RbacPermission::RecordingStop
        )
    {
        PermissionDecisionSource::LegacyMeetingStarter
    } else {
        PermissionDecisionSource::LegacyChannelView
    }
}
