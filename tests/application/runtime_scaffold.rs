use discord_transcript::application::bot::{BotCommandService, StartCommandInput};
use discord_transcript::application::command::PermissionSet;
use discord_transcript::application::runtime::{
    RECORD_START_COMMAND, RECORD_STOP_COMMAND, RuntimeCommandInput, bot_permissions_from_cache_state,
    create_serenity_commands, dispatch_runtime_command, meeting_audio_path, parse_stop_reason,
    run_guild_scoped_command, slash_command_specs, stop_and_enqueue_summary_job,
    validate_command_guild,
};
use discord_transcript::domain::{JobStatus, JobType, MeetingStatus, StopReason};
use discord_transcript::domain::authz::UserRole;
use discord_transcript::infrastructure::queue::{InMemoryJobQueue, Job, JobQueue, QueueError};
use discord_transcript::infrastructure::storage::{
    CreateMeetingRequest, InMemoryMeetingStore, MeetingStore,
};
use serenity::all::GuildId;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct FailingEnqueueQueue;

impl JobQueue for FailingEnqueueQueue {
    fn enqueue(&mut self, _job: Job) -> Result<(), QueueError> {
        Err(QueueError::Backend("enqueue failed".to_owned()))
    }

    fn claim_next(&mut self, _job_type: JobType) -> Result<Option<Job>, QueueError> {
        Ok(None)
    }

    fn claim_by_id(&mut self, _job_id: &str) -> Result<Option<Job>, QueueError> {
        Ok(None)
    }

    fn mark_done(&mut self, _job_id: &str) -> Result<(), QueueError> {
        Ok(())
    }

    fn mark_failed(&mut self, _job_id: &str, _error_message: String) -> Result<(), QueueError> {
        Ok(())
    }

    fn retry(
        &mut self,
        _job_id: &str,
        _error_message: String,
        _max_retries: u32,
    ) -> Result<JobStatus, QueueError> {
        Ok(JobStatus::Queued)
    }
}

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
fn validate_command_guild_rejects_missing_or_wrong_guild() {
    let configured = GuildId::new(123);

    assert_eq!(
        validate_command_guild(None, configured),
        Err("guild_id is required for this command".to_owned())
    );
    assert_eq!(
        validate_command_guild(Some(GuildId::new(456)), configured),
        Err("command is not configured for this guild".to_owned())
    );
    assert_eq!(
        validate_command_guild(Some(configured), configured),
        Ok(configured)
    );
}

#[tokio::test]
async fn run_guild_scoped_command_does_not_invoke_work_for_wrong_guild() {
    let invoked = Arc::new(AtomicBool::new(false));
    let invoked_in_command = Arc::clone(&invoked);

    let message =
        run_guild_scoped_command(Some(GuildId::new(456)), GuildId::new(123), move |_| async move {
            invoked_in_command.store(true, Ordering::SeqCst);
            Ok("started".to_owned())
        })
        .await;

    assert_eq!(message, "error: command is not configured for this guild");
    assert!(!invoked.load(Ordering::SeqCst));
}

#[test]
fn bot_permissions_fail_closed_on_missing_cache_data() {
    assert_eq!(
        bot_permissions_from_cache_state(false, true, Some(true), Some(true)),
        PermissionSet {
            can_connect_voice: false,
            can_send_messages: false,
        }
    );
    assert_eq!(
        bot_permissions_from_cache_state(true, false, Some(true), Some(true)),
        PermissionSet {
            can_connect_voice: false,
            can_send_messages: false,
        }
    );
    assert_eq!(
        bot_permissions_from_cache_state(true, true, None, Some(true)),
        PermissionSet {
            can_connect_voice: false,
            can_send_messages: true,
        }
    );
    assert_eq!(
        bot_permissions_from_cache_state(true, true, Some(true), None),
        PermissionSet {
            can_connect_voice: true,
            can_send_messages: false,
        }
    );
}

#[test]
fn bot_permissions_allow_only_when_all_cache_permissions_are_positive() {
    assert_eq!(
        bot_permissions_from_cache_state(true, true, Some(true), Some(true)),
        PermissionSet {
            can_connect_voice: true,
            can_send_messages: true,
        }
    );
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
            caller_role: UserRole::GuildAdmin,
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
            caller_role: UserRole::GuildAdmin,
        }),
    )
    .expect("start should succeed");

    let stop = stop_and_enqueue_summary_job(
        &mut service,
        &mut queue,
        "g1",
        "u1",
        UserRole::Member,
        Some("m1"),
        StopReason::Manual,
    )
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
            caller_role: UserRole::GuildAdmin,
        }),
    )
    .expect("start should succeed");

    let first = stop_and_enqueue_summary_job(
        &mut service,
        &mut queue,
        "g1",
        "u1",
        UserRole::Member,
        Some("m1"),
        StopReason::Manual,
    )
        .expect("first stop should succeed");
    assert_eq!(first.meeting_id, "m1");

    // After stop, meeting is Stopping but still found by find_active_meeting_by_guild.
    // stop_meeting CAS returns AlreadyHandled (no new job enqueued).
    let second = stop_and_enqueue_summary_job(
        &mut service,
        &mut queue,
        "g1",
        "u1",
        UserRole::Member,
        Some("m1"),
        StopReason::Manual,
    )
        .expect("second stop should succeed (idempotent)");
    assert_eq!(
        second.outcome,
        discord_transcript::application::stop::StopOutcome::AlreadyHandled
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
fn stop_and_enqueue_summary_job_can_recover_after_enqueue_failure() {
    let store = InMemoryMeetingStore::new();
    let mut service = BotCommandService::new(store);

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
            caller_role: UserRole::GuildAdmin,
        }),
    )
    .expect("start should succeed");

    let mut failing_queue = FailingEnqueueQueue;
    let first = stop_and_enqueue_summary_job(
        &mut service,
        &mut failing_queue,
        "g1",
        "u1",
        UserRole::Member,
        Some("m1"),
        StopReason::Manual,
    );
    assert!(first.is_err(), "enqueue failure should be surfaced");
    assert_eq!(
        service.store.get("m1").expect("meeting should exist").status,
        MeetingStatus::Stopping
    );

    let mut queue = InMemoryJobQueue::new();
    let second = stop_and_enqueue_summary_job(
        &mut service,
        &mut queue,
        "g1",
        "u1",
        UserRole::Member,
        Some("m1"),
        StopReason::Manual,
    )
        .expect("retry should enqueue summary for already-stopping meeting");
    assert_eq!(
        second.outcome,
        discord_transcript::application::stop::StopOutcome::AlreadyHandled
    );

    let claimed = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(claimed.meeting_id, "m1");
}

#[test]
fn stop_and_enqueue_summary_job_does_not_enqueue_for_scheduled_abort() {
    let mut store = InMemoryMeetingStore::new();
    store
        .create_scheduled_meeting(CreateMeetingRequest {
            id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc1".to_owned(),
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
        })
        .expect("scheduled meeting should be created");
    let mut service = BotCommandService::new(store);
    let mut queue = InMemoryJobQueue::new();

    let stop = stop_and_enqueue_summary_job(
        &mut service,
        &mut queue,
        "g1",
        "u1",
        UserRole::Member,
        Some("m1"),
        StopReason::Manual,
    )
        .expect("scheduled stop should abort without enqueue");

    assert_eq!(
        stop.outcome,
        discord_transcript::application::stop::StopOutcome::AlreadyHandled
    );
    assert_eq!(
        service.store.get("m1").expect("meeting should exist").status,
        MeetingStatus::Aborted
    );
    assert!(
        queue
            .claim_next(JobType::Summarize)
            .expect("claim should succeed")
            .is_none()
    );
}

#[test]
fn meeting_audio_path_uses_chunk_storage_base() {
    let path = meeting_audio_path("/tmp/chunks", "g1", "vc1", "m1");
    assert!(
        path.ends_with("/tmp/chunks/workspaces/g1/vc1/m1/audio/mixdown.wav")
            || path.ends_with("\\tmp\\chunks\\workspaces\\g1\\vc1\\m1\\audio\\mixdown.wav")
    );
}
