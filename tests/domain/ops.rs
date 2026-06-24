use discord_transcript::application::recovery_runner::{RecoveryEffect, run_recovery};
use discord_transcript::domain::MeetingStatus;
use discord_transcript::domain::audit::{AuditEvent, AuditLog};
use discord_transcript::domain::authz::{
    Action, MemberRoleSource, PermissionDecisionSource, RbacPermission, RbacRoleGrant,
    RbacSubject, UserRole, is_allowed, resolve_rbac_permission,
};
use discord_transcript::domain::plans::{
    PlanQuota, QuotaDimension, QuotaEnforcementMode, QuotaLimit, QuotaPeriod,
};
use discord_transcript::domain::recovery::RecoveryAction;
use discord_transcript::domain::recovery::RecoveryCandidate;
use discord_transcript::domain::recovery::decide_recovery_action;
use discord_transcript::domain::retention::{
    ArtifactRecord, RetentionKind, RetentionPolicy, select_cleanup_candidates,
    should_delete_artifact,
};
use discord_transcript::domain::usage::{
    EntitlementAction, EntitlementEvaluator, EntitlementMode, EntitlementPolicy, NewUsageEvent,
    UsageAggregate, UsageDetailJson, UsageEvent, UsageEventLedger, UsageMetric, UsageSnapshot,
    recording_minutes_from_seconds,
};
use discord_transcript::infrastructure::storage::{
    InMemoryMeetingStore, StoredMeeting, UsageEventStore,
};
use chrono::{TimeZone, Utc};
use std::num::NonZeroU32;

fn nonzero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value should be nonzero")
}

fn recording_meeting(id: &str) -> StoredMeeting {
    StoredMeeting {
        id: id.to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
    voice_channel_name: None,
        report_channel_id: "tc1".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
        duration_seconds: None,
    }
}

#[test]
fn recovery_runner_marks_failed_when_recording_missing_file() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(recording_meeting("m1"));

    let effect = run_recovery(
        &mut store,
        &RecoveryCandidate {
            meeting_id: "m1".to_owned(),
            status: MeetingStatus::Recording,
            voice_connected: false,
            has_local_session: false,
            has_recording_file: false,
        },
    )
    .expect("recovery should work");

    assert_eq!(
        effect,
        RecoveryEffect::MarkedFailed {
            meeting_id: "m1".to_owned()
        }
    );
    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.status, MeetingStatus::Failed);
    assert!(saved.error_message.is_some());
}

#[test]
fn recovery_noops_for_connected_recording_with_local_session() {
    let action = decide_recovery_action(&RecoveryCandidate {
        meeting_id: "m1".to_owned(),
        status: MeetingStatus::Recording,
        voice_connected: true,
        has_local_session: true,
        has_recording_file: false,
    });

    assert_eq!(action, RecoveryAction::Noop);
}

#[test]
fn recovery_stops_connected_recording_without_local_session_when_audio_exists() {
    let action = decide_recovery_action(&RecoveryCandidate {
        meeting_id: "m1".to_owned(),
        status: MeetingStatus::Recording,
        voice_connected: true,
        has_local_session: false,
        has_recording_file: true,
    });

    assert_eq!(action, RecoveryAction::ConfirmStopClientDisconnect);
}

#[test]
fn recovery_fails_connected_recording_without_local_session_when_audio_is_missing() {
    let action = decide_recovery_action(&RecoveryCandidate {
        meeting_id: "m1".to_owned(),
        status: MeetingStatus::Recording,
        voice_connected: true,
        has_local_session: false,
        has_recording_file: false,
    });

    assert_eq!(action, RecoveryAction::MarkFailedMissingRecording);
}

#[test]
fn command_authorization_policies_cover_command_roles() {
    assert!(!is_allowed(UserRole::Member, Action::StartRecording));
    assert!(!is_allowed(UserRole::Member, Action::StopRecording));
    assert!(is_allowed(UserRole::StartedMeeting, Action::StopRecording));
    assert!(!is_allowed(UserRole::StartedMeeting, Action::StartRecording));
    assert!(is_allowed(UserRole::GuildAdmin, Action::StartRecording));
    assert!(is_allowed(UserRole::GuildAdmin, Action::StopRecording));
    assert!(is_allowed(UserRole::BotAdmin, Action::StartRecording));
    assert!(is_allowed(UserRole::BotAdmin, Action::StopRecording));
}

fn grant(guild_id: &str, discord_role_id: &str, permission: RbacPermission) -> RbacRoleGrant {
    RbacRoleGrant {
        guild_id: guild_id.to_owned(),
        discord_role_id: discord_role_id.to_owned(),
        permission,
    }
}

#[test]
fn rbac_resolver_preserves_legacy_channel_view_defaults() {
    let subject = RbacSubject {
        has_channel_view: true,
        ..RbacSubject::default()
    };

    let view = resolve_rbac_permission(
        RbacPermission::MeetingView,
        "g1",
        subject,
        MemberRoleSource::LookupFailed,
        &[],
    );
    assert!(view.allowed);
    assert_eq!(view.source, PermissionDecisionSource::LegacyChannelView);

    let start = resolve_rbac_permission(
        RbacPermission::RecordingStart,
        "g1",
        subject,
        MemberRoleSource::Available(&[]),
        &[],
    );
    assert!(!start.allowed);
    assert_eq!(start.source, PermissionDecisionSource::NoGrant);
}

#[test]
fn rbac_resolver_preserves_legacy_meeting_starter_defaults() {
    let subject = RbacSubject {
        is_meeting_starter: true,
        ..RbacSubject::default()
    };

    let stop = resolve_rbac_permission(
        RbacPermission::RecordingStop,
        "g1",
        subject,
        MemberRoleSource::LookupFailed,
        &[],
    );

    assert!(stop.allowed);
    assert_eq!(stop.source, PermissionDecisionSource::LegacyMeetingStarter);
}

#[test]
fn rbac_resolver_allows_member_role_grant() {
    let roles = vec!["role-recorders".to_owned()];
    let grants = vec![grant(
        "g1",
        "role-recorders",
        RbacPermission::RecordingStart,
    )];

    let decision = resolve_rbac_permission(
        RbacPermission::RecordingStart,
        "g1",
        RbacSubject::default(),
        MemberRoleSource::Available(&roles),
        &grants,
    );

    assert!(decision.allowed);
    assert_eq!(
        decision.source,
        PermissionDecisionSource::RbacRole {
            discord_role_id: "role-recorders".to_owned()
        }
    );
}

#[test]
fn rbac_resolver_unions_multiple_member_roles() {
    let roles = vec!["role-viewers".to_owned(), "role-ops".to_owned()];
    let grants = vec![
        grant("g1", "role-viewers", RbacPermission::MeetingView),
        grant("g1", "role-ops", RbacPermission::MeetingDelete),
    ];

    let decision = resolve_rbac_permission(
        RbacPermission::MeetingDelete,
        "g1",
        RbacSubject::default(),
        MemberRoleSource::Available(&roles),
        &grants,
    );

    assert!(decision.allowed);
    assert_eq!(
        decision.source,
        PermissionDecisionSource::RbacRole {
            discord_role_id: "role-ops".to_owned()
        }
    );
}

#[test]
fn rbac_resolver_ignores_deleted_or_missing_roles() {
    let roles = vec!["role-current".to_owned()];
    let grants = vec![grant("g1", "role-deleted", RbacPermission::MeetingDelete)];

    let decision = resolve_rbac_permission(
        RbacPermission::MeetingDelete,
        "g1",
        RbacSubject::default(),
        MemberRoleSource::Available(&roles),
        &grants,
    );

    assert!(!decision.allowed);
    assert_eq!(decision.source, PermissionDecisionSource::NoGrant);
}

#[test]
fn rbac_resolver_ignores_matching_roles_from_other_guilds() {
    let roles = vec!["role-ops".to_owned()];
    let grants = vec![grant("g2", "role-ops", RbacPermission::MeetingDelete)];

    let decision = resolve_rbac_permission(
        RbacPermission::MeetingDelete,
        "g1",
        RbacSubject::default(),
        MemberRoleSource::Available(&roles),
        &grants,
    );

    assert!(!decision.allowed);
    assert_eq!(decision.source, PermissionDecisionSource::NoGrant);
}

#[test]
fn rbac_resolver_fails_closed_when_member_roles_are_unavailable() {
    let grants = vec![grant("g1", "role-ops", RbacPermission::MeetingDelete)];

    let decision = resolve_rbac_permission(
        RbacPermission::MeetingDelete,
        "g1",
        RbacSubject::default(),
        MemberRoleSource::LookupFailed,
        &grants,
    );

    assert!(!decision.allowed);
    assert_eq!(
        decision.source,
        PermissionDecisionSource::MemberRoleLookupFailed
    );
}

#[test]
fn rbac_resolver_admin_overrides_precede_role_lookup_failures() {
    let bot_admin = resolve_rbac_permission(
        RbacPermission::SettingsManage,
        "g1",
        RbacSubject {
            is_bot_admin: true,
            ..RbacSubject::default()
        },
        MemberRoleSource::LookupFailed,
        &[],
    );
    assert!(bot_admin.allowed);
    assert_eq!(bot_admin.source, PermissionDecisionSource::BotAdmin);

    let guild_admin = resolve_rbac_permission(
        RbacPermission::SettingsManage,
        "g1",
        RbacSubject {
            is_guild_admin: true,
            ..RbacSubject::default()
        },
        MemberRoleSource::LookupFailed,
        &[],
    );
    assert!(guild_admin.allowed);
    assert_eq!(guild_admin.source, PermissionDecisionSource::GuildAdmin);
}

#[test]
fn recovery_runner_requeues_asr_for_stopping_meeting() {
    let mut store = InMemoryMeetingStore::new();
    let mut meeting = recording_meeting("m1");
    meeting.status = MeetingStatus::Stopping;
    store.insert(meeting);

    let effect = run_recovery(
        &mut store,
        &RecoveryCandidate {
            meeting_id: "m1".to_owned(),
            status: MeetingStatus::Stopping,
            voice_connected: false,
            has_local_session: false,
            has_recording_file: true,
        },
    )
    .expect("recovery should work");

    assert_eq!(
        effect,
        RecoveryEffect::SummaryRequeued {
            meeting_id: "m1".to_owned()
        }
    );
    // Status stays Stopping — it advances to Transcribing only when the job
    // is actually claimed and begins processing.
    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.status, MeetingStatus::Stopping);
}

#[test]
fn recovery_requeues_summary_for_stopping_with_recording() {
    // A Stopping meeting with a recording file always gets RequeueSummary.
    // The runtime's enqueue call handles the AlreadyExists case gracefully, so
    // this applies whether or not a summary job was previously queued.
    let action = decide_recovery_action(&RecoveryCandidate {
        meeting_id: "m1".to_owned(),
        status: MeetingStatus::Stopping,
        voice_connected: false,
        has_local_session: false,
        has_recording_file: true,
    });
    assert_eq!(action, RecoveryAction::RequeueSummary);
}

#[test]
fn recovery_resets_transcribing_to_stopping_and_requeues() {
    let mut store = InMemoryMeetingStore::new();
    let mut meeting = recording_meeting("m1");
    meeting.status = MeetingStatus::Transcribing;
    store.insert(meeting);

    let effect = run_recovery(
        &mut store,
        &RecoveryCandidate {
            meeting_id: "m1".to_owned(),
            status: MeetingStatus::Transcribing,
            voice_connected: false,
            has_local_session: false,
            has_recording_file: true,
        },
    )
    .expect("recovery should work");

    assert_eq!(
        effect,
        RecoveryEffect::SummaryRequeued {
            meeting_id: "m1".to_owned()
        }
    );
    // Status should be reset to Stopping so the pipeline can re-drive it
    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.status, MeetingStatus::Stopping);
}

#[test]
fn recovery_resets_summarizing_to_stopping_and_requeues() {
    let mut store = InMemoryMeetingStore::new();
    let mut meeting = recording_meeting("m1");
    meeting.status = MeetingStatus::Summarizing;
    store.insert(meeting);

    let effect = run_recovery(
        &mut store,
        &RecoveryCandidate {
            meeting_id: "m1".to_owned(),
            status: MeetingStatus::Summarizing,
            voice_connected: false,
            has_local_session: false,
            has_recording_file: true,
        },
    )
    .expect("recovery should work");

    assert_eq!(
        effect,
        RecoveryEffect::SummaryRequeued {
            meeting_id: "m1".to_owned()
        }
    );
    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.status, MeetingStatus::Stopping);
}

#[test]
fn recovery_marks_failed_for_transcribing_without_recording() {
    let action = decide_recovery_action(&RecoveryCandidate {
        meeting_id: "m1".to_owned(),
        status: MeetingStatus::Transcribing,
        voice_connected: false,
        has_local_session: false,
        has_recording_file: false,
    });
    assert_eq!(action, RecoveryAction::MarkFailedMissingRecording);
}

#[test]
fn retention_policy_selects_expected_cleanup_targets() {
    let now = 10_000_000u64;
    let policy = RetentionPolicy {
        raw_audio_ttl_days: nonzero(7),
        transcript_ttl_days: nonzero(30),
        summary_ttl_days: Some(nonzero(90)),
    };
    let records = vec![
        ArtifactRecord {
            kind: RetentionKind::RawAudio,
            created_at_unix_seconds: now - 8 * 86_400,
        },
        ArtifactRecord {
            kind: RetentionKind::Transcript,
            created_at_unix_seconds: now - 5 * 86_400,
        },
        ArtifactRecord {
            kind: RetentionKind::Summary,
            created_at_unix_seconds: now - 95 * 86_400,
        },
    ];

    assert!(should_delete_artifact(records[0], now, policy));
    let candidates = select_cleanup_candidates(&records, now, policy);
    assert_eq!(candidates.len(), 2);
}

#[test]
fn access_control_matches_mvp_rules() {
    assert!(is_allowed(UserRole::BotAdmin, Action::Reprocess));
    assert!(is_allowed(UserRole::GuildAdmin, Action::Delete));
    assert!(is_allowed(UserRole::StartedMeeting, Action::Delete));
    assert!(!is_allowed(UserRole::StartedMeeting, Action::Reprocess));
    assert!(is_allowed(UserRole::Member, Action::View));
    assert!(!is_allowed(UserRole::Member, Action::Delete));
}

#[test]
fn audit_log_appends_and_reads_events() {
    let mut log = AuditLog::new();
    let occurred_at = Utc.with_ymd_and_hms(2026, 6, 3, 1, 2, 3).unwrap();
    log.append(AuditEvent {
        id: "audit-1".to_owned(),
        tenant_id: Some("tenant-g1".to_owned()),
        guild_id: Some("g1".to_owned()),
        actor_user_id: Some("u1".to_owned()),
        action: "delete_transcript".to_owned(),
        resource_type: "meeting".to_owned(),
        resource_id: Some("m1".to_owned()),
        request_metadata_json: r#"{"method":"DELETE"}"#.to_owned(),
        detail_json: r#"{"reason":"manual_cleanup"}"#.to_owned(),
        occurred_at,
        created_at: occurred_at,
    });
    assert_eq!(log.list().len(), 1);
    assert_eq!(log.list()[0].action, "delete_transcript");
    assert_eq!(log.recent(1)[0].resource_id.as_deref(), Some("m1"));
}

#[test]
fn usage_event_ledger_is_append_only_and_idempotent_by_event_id() {
    let mut ledger = UsageEventLedger::new();
    let observed_at = Utc.with_ymd_and_hms(2026, 6, 3, 1, 2, 3).unwrap();
    let rounded_minutes = recording_minutes_from_seconds(61);
    let event = UsageEvent {
        id: "usage-1".to_owned(),
        tenant_id: Some("tenant-g1".to_owned()),
        guild_id: "g1".to_owned(),
        meeting_id: Some("m1".to_owned()),
        job_id: None,
        resource_type: Some("meeting".to_owned()),
        resource_id: Some("m1".to_owned()),
        metric: UsageMetric::RecordingMinutes,
        quantity: rounded_minutes,
        detail_json: r#"{"duration_seconds":61}"#.to_owned(),
        observed_at,
        created_at: observed_at,
    };

    ledger.append(event.clone());
    ledger.append(event);

    assert_eq!(ledger.list().len(), 1);
    assert_eq!(ledger.recent(10)[0].quantity, rounded_minutes);
}

#[test]
fn entitlement_evaluator_observe_only_never_blocks() {
    let snapshot = UsageSnapshot::from_aggregates(vec![UsageAggregate {
        metric: UsageMetric::RecordingMinutes,
        quantity: 200,
    }]);
    let policy = EntitlementPolicy {
        recording_minutes_limit: Some(100),
        ..EntitlementPolicy::default()
    };

    let observe_only = EntitlementEvaluator::new(EntitlementMode::ObserveOnly, policy.clone())
        .evaluate(EntitlementAction::StartRecording, &snapshot);
    let enforced = EntitlementEvaluator::new(EntitlementMode::Enforce, policy)
        .evaluate(EntitlementAction::StartRecording, &snapshot);

    assert!(observe_only.allowed);
    assert!(
        observe_only.observations.iter().any(|observation| {
            observation.metric == UsageMetric::RecordingMinutes && observation.exceeded
        })
    );
    assert!(!enforced.allowed);

    let default_observe_only =
        EntitlementEvaluator::observe_only().evaluate(EntitlementAction::StartRecording, &snapshot);
    assert!(default_observe_only.allowed);
    assert_eq!(default_observe_only.observations.len(), 5);
}

#[test]
fn plan_quota_limit_validation_distinguishes_unlimited_and_finite_limits() {
    let finite = PlanQuota::from_parts(
        "quota-finite".to_owned(),
        QuotaDimension::RecordingMinutes,
        QuotaPeriod::Monthly,
        false,
        Some(100),
        QuotaEnforcementMode::ObserveOnly,
    )
    .expect("finite quota should be valid");
    let unlimited = PlanQuota::from_parts(
        "quota-unlimited".to_owned(),
        QuotaDimension::StorageBytes,
        QuotaPeriod::Current,
        true,
        None,
        QuotaEnforcementMode::Enforce,
    )
    .expect("unlimited quota should be valid");

    assert_eq!(finite.limit, QuotaLimit::Finite(100));
    assert_eq!(finite.limit.limit_value(), Some(100));
    assert!(!finite.limit.is_unlimited());
    assert_eq!(unlimited.limit, QuotaLimit::Unlimited);
    assert_eq!(unlimited.limit.limit_value(), None);
    assert!(unlimited.limit.is_unlimited());
    assert!(
        PlanQuota::from_parts(
            "quota-invalid".to_owned(),
            QuotaDimension::SummaryRuns,
            QuotaPeriod::Monthly,
            false,
            None,
            QuotaEnforcementMode::ObserveOnly,
        )
        .is_err()
    );
    assert!(
        PlanQuota::from_parts(
            "quota-invalid".to_owned(),
            QuotaDimension::SummaryRuns,
            QuotaPeriod::Monthly,
            true,
            Some(10),
            QuotaEnforcementMode::ObserveOnly,
        )
        .is_err()
    );
}

#[test]
fn usage_detail_json_accepts_only_json_objects() {
    assert!(UsageDetailJson::parse(r#"{"source":"test"}"#).is_ok());
    assert!(UsageDetailJson::parse("[]").is_err());
    assert!(UsageDetailJson::parse("").is_err());
}

#[test]
fn in_memory_usage_aggregate_honors_recent_window() {
    let mut store = InMemoryMeetingStore::new();
    let now = Utc::now();
    for (id, observed_at) in [
        ("old", now - chrono::Duration::seconds(3_600)),
        ("recent", now - chrono::Duration::seconds(30)),
    ] {
        store
            .append_usage_event(&NewUsageEvent {
                id: format!("usage-{id}"),
                tenant_id: None,
                guild_id: "g1".to_owned(),
                meeting_id: Some("m1".to_owned()),
                job_id: None,
                resource_type: Some("meeting".to_owned()),
                resource_id: Some("m1".to_owned()),
                metric: UsageMetric::DebugDownloads,
                quantity: 1,
                detail_json: UsageDetailJson::parse("{}").expect("usage detail should parse"),
                observed_at,
            })
            .expect("usage event should append");
    }

    let aggregate = store
        .aggregate_recent_usage(None, Some("g1"), 60)
        .expect("usage aggregate should succeed");

    assert_eq!(aggregate.len(), 1);
    assert_eq!(aggregate[0].quantity, 1);
}

#[test]
fn in_memory_usage_tenant_filter_includes_guild_scoped_events() {
    let mut store = InMemoryMeetingStore::new();
    store
        .append_usage_event(&NewUsageEvent {
            id: "usage-guild-scoped".to_owned(),
            tenant_id: None,
            guild_id: "g1".to_owned(),
            meeting_id: Some("m1".to_owned()),
            job_id: None,
            resource_type: Some("meeting".to_owned()),
            resource_id: Some("m1".to_owned()),
            metric: UsageMetric::SummaryRuns,
            quantity: 1,
            detail_json: UsageDetailJson::parse("{}").expect("usage detail should parse"),
            observed_at: Utc::now(),
        })
        .expect("usage event should append");

    let by_tenant_and_guild = store
        .list_recent_usage_events(Some("tenant-g1"), Some("g1"), 500)
        .expect("usage list should succeed");
    let by_tenant_only = store
        .list_recent_usage_events(Some("tenant-g1"), None, 500)
        .expect("usage list should succeed");

    assert_eq!(by_tenant_and_guild.len(), 1);
    assert!(by_tenant_only.is_empty());
}
