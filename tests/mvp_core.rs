use discord_transcript::domain::{MeetingStatus, StopReason};
use discord_transcript::posting::{
    DISCORD_MESSAGE_LIMIT, TranscriptDelivery, decide_transcript_delivery, split_discord_message,
};
use discord_transcript::recovery::{RecoveryAction, RecoveryCandidate, decide_recovery_action};
use discord_transcript::stop::{StopOutcome, stop_meeting};
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
fn stop_meeting_acquires_owner_on_first_stop() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(recording_meeting("m1"));

    let outcome = stop_meeting(&mut store, "m1", StopReason::Manual).expect("stop should succeed");
    assert_eq!(outcome, StopOutcome::Owner);

    let stored = store.get("m1").expect("meeting should exist");
    assert_eq!(stored.status, MeetingStatus::Stopping);
    assert_eq!(stored.stop_reason, Some(StopReason::Manual));
}

#[test]
fn stop_meeting_is_idempotent_when_called_again() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(recording_meeting("m1"));

    let first = stop_meeting(&mut store, "m1", StopReason::Manual).expect("first stop should work");
    let second =
        stop_meeting(&mut store, "m1", StopReason::AutoEmpty).expect("second stop should work");

    assert_eq!(first, StopOutcome::Owner);
    assert_eq!(second, StopOutcome::AlreadyHandled);
}

#[test]
fn recovery_decision_matches_mvp_rules() {
    let recording_disconnected_with_audio = RecoveryCandidate {
        meeting_id: "m1".to_owned(),
        status: MeetingStatus::Recording,
        voice_connected: false,
        has_recording_file: true,
        summary_job_already_queued: false,
    };
    assert_eq!(
        decide_recovery_action(&recording_disconnected_with_audio),
        RecoveryAction::ConfirmStopClientDisconnect
    );

    let stopping_missing_asr_job = RecoveryCandidate {
        meeting_id: "m2".to_owned(),
        status: MeetingStatus::Stopping,
        voice_connected: false,
        has_recording_file: true,
        summary_job_already_queued: false,
    };
    assert_eq!(
        decide_recovery_action(&stopping_missing_asr_job),
        RecoveryAction::RequeueSummary
    );

    let recording_missing_audio = RecoveryCandidate {
        meeting_id: "m3".to_owned(),
        status: MeetingStatus::Recording,
        voice_connected: false,
        has_recording_file: false,
        summary_job_already_queued: false,
    };
    assert_eq!(
        decide_recovery_action(&recording_missing_audio),
        RecoveryAction::MarkFailedMissingRecording
    );
}

#[test]
fn discord_message_split_respects_character_limit() {
    let text = format!(
        "{}\n{}",
        "a".repeat(DISCORD_MESSAGE_LIMIT),
        "b".repeat(DISCORD_MESSAGE_LIMIT + 20)
    );

    let chunks = split_discord_message(&text, DISCORD_MESSAGE_LIMIT);
    assert!(chunks.len() >= 2);
    for chunk in &chunks {
        assert!(chunk.chars().count() <= DISCORD_MESSAGE_LIMIT);
    }
    assert_eq!(chunks.concat(), text);
}

#[test]
fn transcript_delivery_falls_back_to_link_when_too_large() {
    assert_eq!(
        decide_transcript_delivery(1024, 2 * 1024),
        TranscriptDelivery::AttachTextFile
    );
    assert_eq!(
        decide_transcript_delivery(3 * 1024, 2 * 1024),
        TranscriptDelivery::ShareLinkOnly
    );
}
