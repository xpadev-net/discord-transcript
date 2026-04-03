use discord_transcript::audit::{AuditEvent, AuditLog};
use discord_transcript::authz::{Action, UserRole, is_allowed};
use discord_transcript::domain::MeetingStatus;
use discord_transcript::recovery::RecoveryAction;
use discord_transcript::recovery::RecoveryCandidate;
use discord_transcript::recovery::decide_recovery_action;
use discord_transcript::recovery_runner::{RecoveryEffect, run_recovery};
use discord_transcript::retention::{
    ArtifactRecord, RetentionKind, RetentionPolicy, select_cleanup_candidates,
    should_delete_artifact,
};
use discord_transcript::storage::{InMemoryMeetingStore, StoredMeeting};

fn recording_meeting(id: &str) -> StoredMeeting {
    StoredMeeting {
        id: id.to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        report_channel_id: "tc1".to_owned(),
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
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
        has_recording_file: false,
    });
    assert_eq!(action, RecoveryAction::MarkFailedMissingRecording);
}

#[test]
fn retention_policy_selects_expected_cleanup_targets() {
    let now = 10_000_000u64;
    let policy = RetentionPolicy {
        raw_audio_ttl_days: 7,
        transcript_ttl_days: 30,
        summary_ttl_days: Some(90),
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
    log.append(AuditEvent {
        actor_user_id: "u1".to_owned(),
        action: "delete_transcript".to_owned(),
        meeting_id: "m1".to_owned(),
        detail: "manual cleanup".to_owned(),
    });
    assert_eq!(log.list().len(), 1);
    assert_eq!(log.list()[0].action, "delete_transcript");
}
