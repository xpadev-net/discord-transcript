use std::collections::HashSet;
use std::fmt;
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

macro_rules! define_rbac_permissions {
    ($(($variant:ident, $name:literal)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum RbacPermission {
            $($variant),+
        }

        impl RbacPermission {
            pub const ALL: [Self; define_rbac_permissions!(@count $($variant),+)] = [
                $(Self::$variant),+
            ];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }

        impl FromStr for RbacPermission {
            type Err = RbacPermissionParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($name => Ok(Self::$variant),)+
                    _ => Err(RbacPermissionParseError),
                }
            }
        }
    };
    (@count $($variant:ident),+) => {
        <[()]>::len(&[$(define_rbac_permissions!(@unit $variant)),+])
    };
    (@unit $variant:ident) => {
        ()
    };
}

define_rbac_permissions!(
    (RecordingStart, "recording:start"),
    (RecordingStop, "recording:stop"),
    (MeetingView, "meeting:view"),
    (MeetingReprocess, "meeting:reprocess"),
    (MeetingDelete, "meeting:delete"),
    (SettingsManage, "settings:manage"),
    (SummaryTemplateManage, "summary_template:manage"),
    (DomainKnowledgeManage, "domain_knowledge:manage"),
    (UsageView, "usage:view"),
    (AdminView, "admin:view"),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RbacPermissionParseError;

impl fmt::Display for RbacPermissionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown RBAC permission")
    }
}

impl std::error::Error for RbacPermissionParseError {}

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
    let Some(action) = legacy_action_for_permission(permission) else {
        return false;
    };
    (subject.is_meeting_starter && is_allowed(UserRole::StartedMeeting, action))
        || (subject.has_channel_view && is_allowed(UserRole::Member, action))
}

fn legacy_access_source(
    subject: RbacSubject,
    permission: RbacPermission,
) -> PermissionDecisionSource {
    let Some(action) = legacy_action_for_permission(permission) else {
        unreachable!(
            "legacy_access_source called for permission {:?} that has no legacy action; \
             caller must guard with legacy_access_allows",
            permission
        );
    };
    if subject.is_meeting_starter && is_allowed(UserRole::StartedMeeting, action) {
        PermissionDecisionSource::LegacyMeetingStarter
    } else {
        debug_assert!(
            subject.has_channel_view && is_allowed(UserRole::Member, action),
            "legacy_access_source called for a subject without legacy access"
        );
        PermissionDecisionSource::LegacyChannelView
    }
}

fn legacy_action_for_permission(permission: RbacPermission) -> Option<Action> {
    match permission {
        RbacPermission::RecordingStart => Some(Action::StartRecording),
        RbacPermission::RecordingStop => Some(Action::StopRecording),
        RbacPermission::MeetingView => Some(Action::View),
        RbacPermission::MeetingReprocess => Some(Action::Reprocess),
        RbacPermission::MeetingDelete => Some(Action::Delete),
        RbacPermission::SettingsManage
        | RbacPermission::SummaryTemplateManage
        | RbacPermission::DomainKnowledgeManage
        | RbacPermission::UsageView
        | RbacPermission::AdminView => None,
    }
}
