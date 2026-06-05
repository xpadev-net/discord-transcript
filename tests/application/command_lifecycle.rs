use discord_transcript::application::auto_stop::{AutoStopSignal, AutoStopState};
use discord_transcript::application::command::{
    CommandError, PermissionSet, RecordStartRequest, RecordStopRequest, record_start, record_stop,
    validate_record_start_preconditions,
};
use discord_transcript::application::stop::StopOutcome;
use discord_transcript::domain::MeetingStatus;
use discord_transcript::domain::StopReason;
use discord_transcript::domain::authz::UserRole;
use discord_transcript::infrastructure::storage::{InMemoryMeetingStore, StoredMeeting};
use std::time::Duration;

fn default_permissions() -> PermissionSet {
    PermissionSet {
        can_connect_voice: true,
        can_send_messages: true,
    }
}

#[test]
fn record_start_persists_report_channel_and_moves_to_recording() {
    let mut store = InMemoryMeetingStore::new();
    let request = RecordStartRequest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u1".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: Some("vc-1".to_owned()),
        permissions: default_permissions(),
        caller_role: UserRole::GuildAdmin,
            effective_settings: None,
    };

    let result = record_start(&mut store, request).expect("start should succeed");
    assert_eq!(result.report_channel_id, "report-chan");
    assert_eq!(result.voice_channel_id, "vc-1");

    let saved = store.get("m1").expect("meeting should be saved");
    assert_eq!(saved.status, MeetingStatus::Recording);
    assert_eq!(saved.report_channel_id, "report-chan");
    assert_eq!(saved.voice_channel_id, "vc-1");
}

#[test]
fn record_start_rejects_when_user_not_in_voice() {
    let mut store = InMemoryMeetingStore::new();
    let request = RecordStartRequest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u1".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: None,
        permissions: default_permissions(),
        caller_role: UserRole::GuildAdmin,
            effective_settings: None,
    };

    let error = record_start(&mut store, request).expect_err("must fail");
    assert_eq!(error, CommandError::UserNotInVoice);
}

#[test]
fn record_start_rejects_if_active_meeting_exists() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "existing".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc-1".to_owned(),
        report_channel_id: "report-chan".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
    });

    let request = RecordStartRequest {
        meeting_id: "new".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u2".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: Some("vc-2".to_owned()),
        permissions: default_permissions(),
        caller_role: UserRole::GuildAdmin,
            effective_settings: None,
    };

    let error = record_start(&mut store, request).expect_err("must fail");
    assert_eq!(
        error,
        CommandError::ActiveMeetingExists {
            meeting_id: "existing".to_owned()
        }
    );
}

#[test]
fn record_start_rejects_plain_member() {
    let mut store = InMemoryMeetingStore::new();
    let request = RecordStartRequest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u1".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: Some("vc-1".to_owned()),
        permissions: default_permissions(),
        caller_role: UserRole::Member,
            effective_settings: None,
    };

    let error = record_start(&mut store, request).expect_err("member must not start");
    assert_eq!(error, CommandError::Unauthorized("start recording"));
}

#[test]
fn record_start_preflight_rejects_plain_member_without_creating_meeting() {
    let mut store = InMemoryMeetingStore::new();
    let request = RecordStartRequest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u1".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: Some("vc-1".to_owned()),
        permissions: default_permissions(),
        caller_role: UserRole::Member,
        effective_settings: None,
    };

    let error =
        validate_record_start_preconditions(&mut store, &request).expect_err("must fail");
    assert_eq!(error, CommandError::Unauthorized("start recording"));
    assert!(store.get("m1").is_none(), "preflight must not create rows");
}

#[test]
fn record_start_preflight_rejects_active_meeting_without_creating_requested_meeting() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "existing".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc-1".to_owned(),
        report_channel_id: "report-chan".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
    });
    let request = RecordStartRequest {
        meeting_id: "new".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u2".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: Some("vc-2".to_owned()),
        permissions: default_permissions(),
        caller_role: UserRole::GuildAdmin,
        effective_settings: None,
    };

    let error =
        validate_record_start_preconditions(&mut store, &request).expect_err("must fail");
    assert_eq!(
        error,
        CommandError::ActiveMeetingExists {
            meeting_id: "existing".to_owned()
        }
    );
    assert!(
        store.get("new").is_none(),
        "preflight must not create the requested row"
    );
}

#[test]
fn record_start_allows_when_previous_meeting_is_stopping() {
    // A meeting in Stopping state (summary processing in progress) should NOT block
    // a new recording, since the voice channel is no longer occupied.
    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "stopping-meeting".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc-1".to_owned(),
        report_channel_id: "report-chan".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Stopping,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
    });

    let request = RecordStartRequest {
        meeting_id: "new".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u2".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: Some("vc-2".to_owned()),
        permissions: default_permissions(),
        caller_role: UserRole::GuildAdmin,
            effective_settings: None,
    };

    let result = record_start(&mut store, request)
        .expect("should succeed while previous meeting is stopping");
    assert_eq!(result.meeting_id, "new");
}

#[test]
fn record_stop_is_idempotent_for_same_meeting() {
    use discord_transcript::application::stop::stop_meeting;

    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc-1".to_owned(),
        report_channel_id: "report-chan".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
    });

    // First stop via command should succeed
    let first = record_stop(
        &mut store,
        RecordStopRequest {
            guild_id: "g1".to_owned(),
            caller_user_id: "u1".to_owned(),
            caller_role: UserRole::Member,
            reason: StopReason::Manual,
        },
    )
    .expect("first stop should pass");
    assert_eq!(first.outcome, StopOutcome::Owner);

    // After stop, meeting is in Stopping but still found by find_active_meeting_by_guild.
    // stop_meeting CAS returns AlreadyHandled, so record_stop is idempotent.
    let second = record_stop(
        &mut store,
        RecordStopRequest {
            guild_id: "g1".to_owned(),
            caller_user_id: "u1".to_owned(),
            caller_role: UserRole::Member,
            reason: StopReason::AutoEmpty,
        },
    )
    .expect("second stop should succeed (idempotent)");
    assert_eq!(second.outcome, StopOutcome::AlreadyHandled);

    // Direct stop_meeting on the same meeting_id is also idempotent via CAS
    let direct =
        stop_meeting(&mut store, "m1", StopReason::AutoEmpty).expect("direct stop should pass");
    assert_eq!(direct, StopOutcome::AlreadyHandled);

    // Verify original stop_reason was preserved
    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.stop_reason, Some(StopReason::Manual));
}

#[test]
fn record_stop_rejects_non_starter_member() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc-1".to_owned(),
        report_channel_id: "report-chan".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "starter".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
    });

    let error = record_stop(
        &mut store,
        RecordStopRequest {
            guild_id: "g1".to_owned(),
            caller_user_id: "other-member".to_owned(),
            caller_role: UserRole::Member,
            reason: StopReason::Manual,
        },
    )
    .expect_err("non-starter member must not stop");
    assert_eq!(error, CommandError::Unauthorized("stop recording"));
}

#[test]
fn record_stop_rejects_started_meeting_role_without_matching_user() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc-1".to_owned(),
        report_channel_id: "report-chan".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "starter".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
    });

    let error = record_stop(
        &mut store,
        RecordStopRequest {
            guild_id: "g1".to_owned(),
            caller_user_id: "other-member".to_owned(),
            caller_role: UserRole::StartedMeeting,
            reason: StopReason::Manual,
        },
    )
    .expect_err("StartedMeeting role still requires ownership");
    assert_eq!(error, CommandError::Unauthorized("stop recording"));
}

#[test]
fn auto_stop_triggers_after_grace_period_and_can_cancel() {
    let mut state = AutoStopState::new(Duration::from_secs(60));
    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::StartTimer
    );
    state.set_empty_since_elapsed_for_test(Duration::from_secs(59));
    assert_eq!(state.tick(), AutoStopSignal::Idle);
    state.set_empty_since_elapsed_for_test(Duration::from_secs(60));
    assert_eq!(state.tick(), AutoStopSignal::Trigger);

    state.clear_timer_active();

    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::StartTimer
    );
    assert_eq!(
        state.on_non_bot_member_count_changed(1),
        AutoStopSignal::Cancelled
    );
    assert_eq!(state.tick(), AutoStopSignal::Idle);
}

#[test]
fn auto_stop_allows_new_timer_after_members_return_at_fire_time() {
    // Simulates the runtime's grace-expiry path when a prior cache miss caused
    // voice_state_update to skip cancelling the timer. At fire time the runtime
    // re-checks member count, sees members returned, feeds that back into the
    // state machine, and must allow a fresh timer for a subsequent empty episode.
    let mut state = AutoStopState::new(Duration::from_secs(60));
    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::StartTimer
    );
    assert_eq!(
        state.on_non_bot_member_count_changed(2),
        AutoStopSignal::Cancelled
    );

    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::StartTimer
    );
    state.set_empty_since_elapsed_for_test(Duration::from_secs(60));
    assert_eq!(state.tick(), AutoStopSignal::Trigger);
}

#[test]
fn auto_stop_rearms_after_failed_stop_attempt() {
    let mut state = AutoStopState::new(Duration::from_secs(60));
    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::StartTimer
    );
    state.set_empty_since_elapsed_for_test(Duration::from_secs(61));
    assert_eq!(state.tick(), AutoStopSignal::Trigger);

    state.retry_after_failed_stop();
    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::AlreadyWaiting
    );
    state.set_empty_since_elapsed_for_test(Duration::from_secs(59));
    assert_eq!(state.tick(), AutoStopSignal::Idle);
    state.set_empty_since_elapsed_for_test(Duration::from_secs(60));
    assert_eq!(state.tick(), AutoStopSignal::Trigger);
}

#[test]
fn auto_stop_grace_uses_monotonic_elapsed_not_wall_clock_ms() {
    let mut state = AutoStopState::new(Duration::from_secs(60));
    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::StartTimer
    );
    state.set_empty_since_elapsed_for_test(Duration::from_secs(60));
    assert_eq!(state.tick(), AutoStopSignal::Trigger);
}

#[test]
fn auto_stop_state_tracks_optional_meeting_id() {
    let unscoped = AutoStopState::new(Duration::from_secs(60));
    assert_eq!(unscoped.meeting_id(), None);
    assert!(unscoped.belongs_to_meeting("m2"));

    let scoped = AutoStopState::new_for_meeting(Duration::from_secs(60), Some("m1".to_owned()));
    assert_eq!(scoped.meeting_id(), Some("m1"));
    assert!(scoped.belongs_to_meeting("m1"));
    assert!(!scoped.belongs_to_meeting("m2"));
}

#[test]
fn auto_stop_unscoped_state_refreshes_for_known_meeting() {
    let mut state = AutoStopState::new(Duration::from_secs(60));
    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::StartTimer
    );
    assert_eq!(state.timer_generation(), 1);
    assert_eq!(state.meeting_id(), None);

    assert!(state.refresh_for_meeting(Duration::from_secs(60), "m1"));

    assert_eq!(state.meeting_id(), Some("m1"));
    assert_eq!(state.timer_generation(), 0);
    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::StartTimer
    );
    assert_eq!(state.timer_generation(), 1);

    assert!(!state.refresh_for_meeting(Duration::from_secs(60), "m1"));
    assert_eq!(
        state.on_non_bot_member_count_changed(0),
        AutoStopSignal::AlreadyWaiting
    );
    assert_eq!(state.timer_generation(), 1);
}
