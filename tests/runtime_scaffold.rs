use discord_transcript::bot::{BotCommandService, StartCommandInput};
use discord_transcript::command::PermissionSet;
use discord_transcript::domain::{JobType, StopReason};
use discord_transcript::queue::{InMemoryJobQueue, JobQueue};
use discord_transcript::runtime::{
    RECORD_START_COMMAND, RECORD_STOP_COMMAND, RuntimeCommandInput, create_serenity_commands,
    dispatch_runtime_command, meeting_audio_path, parse_stop_reason, slash_command_specs,
    stop_and_enqueue_summary_job,
};
use discord_transcript::storage::InMemoryMeetingStore;

#[test]
fn slash_command_specs_match_expected_names() {
    let specs = slash_command_specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].name, RECORD_START_COMMAND);
    assert_eq!(specs[1].name, RECORD_STOP_COMMAND);

    let builders = create_serenity_commands();
    assert_eq!(builders.len(), 2);
}

#[test]
fn runtime_dispatch_routes_record_start() {
    let store = InMemoryMeetingStore::new();
    let mut service = BotCommandService::new(store);

    let result = dispatch_runtime_command(
        &mut service,
        RuntimeCommandInput::RecordStart(StartCommandInput {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            user_id: "u1".to_owned(),
            command_channel_id: "c1".to_owned(),
            user_voice_channel_id: Some("vc1".to_owned()),
            permissions: PermissionSet {
                can_connect_voice: true,
                can_send_messages: true,
            },
        }),
    )
    .expect("dispatch should succeed");

    assert!(result.contains("meeting_id=m1"));
}

#[test]
fn parse_stop_reason_rejects_unknown_values() {
    assert_eq!(
        parse_stop_reason("manual").expect("manual should parse"),
        discord_transcript::domain::StopReason::Manual
    );
    assert!(parse_stop_reason("unknown").is_err());
}

#[test]
fn stop_and_enqueue_summary_job_enqueues_on_owner_stop() {
    let store = InMemoryMeetingStore::new();
    let mut service = BotCommandService::new(store);
    let mut queue = InMemoryJobQueue::new();

    dispatch_runtime_command(
        &mut service,
        RuntimeCommandInput::RecordStart(StartCommandInput {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            user_id: "u1".to_owned(),
            command_channel_id: "c1".to_owned(),
            user_voice_channel_id: Some("vc1".to_owned()),
            permissions: PermissionSet {
                can_connect_voice: true,
                can_send_messages: true,
            },
        }),
    )
    .expect("start should succeed");

    let stop = stop_and_enqueue_summary_job(&mut service, &mut queue, "g1", StopReason::Manual)
        .expect("stop and enqueue should succeed");
    assert_eq!(stop.meeting_id, "m1");

    let claimed = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(claimed.meeting_id, "m1");
}

#[test]
fn stop_and_enqueue_summary_job_is_idempotent_for_queueing() {
    let store = InMemoryMeetingStore::new();
    let mut service = BotCommandService::new(store);
    let mut queue = InMemoryJobQueue::new();

    dispatch_runtime_command(
        &mut service,
        RuntimeCommandInput::RecordStart(StartCommandInput {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            user_id: "u1".to_owned(),
            command_channel_id: "c1".to_owned(),
            user_voice_channel_id: Some("vc1".to_owned()),
            permissions: PermissionSet {
                can_connect_voice: true,
                can_send_messages: true,
            },
        }),
    )
    .expect("start should succeed");

    let first = stop_and_enqueue_summary_job(&mut service, &mut queue, "g1", StopReason::Manual)
        .expect("first stop should succeed");
    assert_eq!(first.meeting_id, "m1");

    // After stop, meeting is Stopping but still found by find_active_meeting_by_guild.
    // stop_meeting CAS returns AlreadyHandled (no new job enqueued).
    let second = stop_and_enqueue_summary_job(&mut service, &mut queue, "g1", StopReason::Manual)
        .expect("second stop should succeed (idempotent)");
    assert_eq!(
        second.outcome,
        discord_transcript::stop::StopOutcome::AlreadyHandled
    );

    // Only one job should be enqueued
    let first_job = queue
        .claim_next(JobType::Summarize)
        .expect("first claim should succeed");
    assert!(first_job.is_some());
    let second_job = queue
        .claim_next(JobType::Summarize)
        .expect("second claim should succeed");
    assert!(second_job.is_none());
}

#[test]
fn meeting_audio_path_uses_chunk_storage_base() {
    let path = meeting_audio_path("/tmp/chunks", "g1", "vc1", "m1");
    assert!(
        path.ends_with("/tmp/chunks/workspaces/g1/vc1/m1/audio/mixdown.wav")
            || path.ends_with("\\tmp\\chunks\\workspaces\\g1\\vc1\\m1\\audio\\mixdown.wav")
    );
}
