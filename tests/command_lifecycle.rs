use discord_transcript::auto_stop::{AutoStopSignal, AutoStopState};
use discord_transcript::command::{
    CommandError, PermissionSet, RecordStartRequest, RecordStopRequest, record_start, record_stop,
};
use discord_transcript::domain::StopReason;
use discord_transcript::stop::StopOutcome;
use discord_transcript::domain::MeetingStatus;
use discord_transcript::storage::{InMemoryMeetingStore, StoredMeeting};
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
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
    });

    let request = RecordStartRequest {
        meeting_id: "new".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u2".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: Some("vc-2".to_owned()),
        permissions: default_permissions(),
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
fn record_start_rejects_if_stopping_meeting_exists() {
    // A meeting in Stopping state (processing in progress) must also block a new start
    // to prevent parallel recordings in the same guild.
    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "stopping-meeting".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc-1".to_owned(),
        report_channel_id: "report-chan".to_owned(),
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Stopping,
        stop_reason: None,
        error_message: None,
    });

    let request = RecordStartRequest {
        meeting_id: "new".to_owned(),
        guild_id: "g1".to_owned(),
        started_by_user_id: "u2".to_owned(),
        command_channel_id: "report-chan".to_owned(),
        user_voice_channel_id: Some("vc-2".to_owned()),
        permissions: default_permissions(),
    };

    let error = record_start(&mut store, request).expect_err("must fail while meeting is stopping");
    assert_eq!(
        error,
        CommandError::ActiveMeetingExists {
            meeting_id: "stopping-meeting".to_owned()
        }
    );
}

#[test]
fn record_stop_is_idempotent_for_same_meeting() {
    use discord_transcript::stop::stop_meeting;

    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc-1".to_owned(),
        report_channel_id: "report-chan".to_owned(),
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
    });

    // First stop via command should succeed
    let first = record_stop(
        &mut store,
        RecordStopRequest {
            guild_id: "g1".to_owned(),
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
            reason: StopReason::AutoEmpty,
        },
    )
    .expect("second stop should succeed (idempotent)");
    assert_eq!(second.outcome, StopOutcome::AlreadyHandled);

    // Direct stop_meeting on the same meeting_id is also idempotent via CAS
    let direct = stop_meeting(&mut store, "m1", StopReason::AutoEmpty)
        .expect("direct stop should pass");
    assert_eq!(direct, StopOutcome::AlreadyHandled);

    // Verify original stop_reason was preserved
    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.stop_reason, Some(StopReason::Manual));
}

#[test]
fn auto_stop_triggers_after_grace_period_and_can_cancel() {
    let mut state = AutoStopState::new(Duration::from_secs(15));
    assert_eq!(
        state.on_non_bot_member_count_changed(0, 1_000),
        AutoStopSignal::Pending
    );
    assert_eq!(state.tick(1_000 + 14_000), AutoStopSignal::Pending);
    assert_eq!(state.tick(1_000 + 15_000), AutoStopSignal::Trigger);

    // second cycle: empty -> rejoin cancels auto stop
    assert_eq!(
        state.on_non_bot_member_count_changed(0, 20_000),
        AutoStopSignal::Pending
    );
    assert_eq!(
        state.on_non_bot_member_count_changed(1, 25_000),
        AutoStopSignal::Cancelled
    );
    assert_eq!(state.tick(40_000), AutoStopSignal::Pending);
}
