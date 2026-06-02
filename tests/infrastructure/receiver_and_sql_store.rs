use discord_transcript::audio::receiver::{BufferedFrame, ReceiverConfig, ReceiverState};
use discord_transcript::domain::MeetingStatus;
use discord_transcript::domain::StopReason;
use discord_transcript::domain::{JobStatus, JobType};
use discord_transcript::infrastructure::queue::JobQueue;
use discord_transcript::infrastructure::sql::INITIAL_SCHEMA_SQL;
use discord_transcript::infrastructure::sql::{
    CLAIM_JOB_SQL, RETRY_JOB_SQL, SET_MEETING_STATUS_CAS_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlJobQueue, SqlMeetingStore, sql_row_from_strings,
};
use discord_transcript::infrastructure::storage::{
    CreateMeetingRequest, MeetingStore, StoreError, StoredMeeting,
};
use std::time::{Duration, Instant};

#[test]
fn receiver_state_flushes_by_chunk_duration() {
    let mut state = ReceiverState::default();
    let config = ReceiverConfig {
        chunk_duration: Duration::from_secs(20),
        silence_flush_duration: Duration::from_secs(30),
        };

    let start = Instant::now();
    state.track_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![1, 2, 3],
        },
    );
    assert!(
        state
            .users_ready_to_flush(start + Duration::from_millis(19_999), &config)
            .is_empty()
    );
    assert_eq!(
        state.users_ready_to_flush(start + Duration::from_secs(21), &config),
        vec!["u1"]
    );

    let chunk = state.take_user_chunk("u1").expect("chunk should exist");
    assert_eq!(chunk.frames.len(), 1);
    assert_eq!(chunk.start_ms, 1_000);
    assert!(state.take_user_chunk("u1").is_none());
}

#[test]
fn sql_store_applies_migration_and_writes_sql() {
    let executor = FakeSqlExecutor::default();
    let mut store = SqlMeetingStore::new(executor);
    store
        .apply_initial_migration(INITIAL_SCHEMA_SQL)
        .expect("migration should execute");

    store
        .create_scheduled_meeting(CreateMeetingRequest {
            id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc1".to_owned(),
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
            effective_settings: None,
        })
        .expect("insert should execute");
    store
        .set_meeting_status("m1", MeetingStatus::Recording, None)
        .expect("status update should execute");
    let transition = store
        .mark_stopping_if_recording("m1", StopReason::Manual)
        .expect("stop transition should execute");
    assert_eq!(
        transition,
        discord_transcript::infrastructure::storage::StopTransition::Acquired
    );

    assert!(!store.executor.executed.is_empty());
}

#[test]
fn sql_store_can_read_active_meeting_from_executor_snapshot() {
    let mut executor = FakeSqlExecutor::default();
    executor.active_by_guild.insert(
        "g1".to_owned(),
        StoredMeeting {
            id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc1".to_owned(),
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
            title: None,
            status: MeetingStatus::Recording,
            stop_reason: None,
            error_message: None,
            started_at: None,
            stopped_at: None,
        },
    );

    let mut store = SqlMeetingStore::new(executor);
    let active = store
        .find_active_meeting_by_guild("g1")
        .expect("query should not fail")
        .expect("active should be returned");
    assert_eq!(active.id, "m1");
}

#[test]
fn sql_store_get_meeting_rejects_unknown_status() {
    let mut executor = FakeSqlExecutor::default();
    let query_sql = "SELECT id, guild_id, voice_channel_id, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, \
                        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
                        to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as stopped_at \
                  FROM meetings WHERE id=$1 LIMIT 1";
    let mut corrupt_status_row = meeting_row_for_title_test(None);
    corrupt_status_row[8] = Some("corrupt".to_owned());
    executor
        .query_rows_result
        .insert(format!("{query_sql}|m1"), vec![corrupt_status_row]);

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .get_meeting("m1")
        .expect_err("unknown status should fail");
    assert!(err.to_string().contains("invalid meeting status"));
}

#[test]
fn sql_store_get_meeting_rejects_unknown_stop_reason() {
    let mut executor = FakeSqlExecutor::default();
    let query_sql = "SELECT id, guild_id, voice_channel_id, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, \
                        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
                        to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as stopped_at \
                  FROM meetings WHERE id=$1 LIMIT 1";
    let mut row = meeting_row_for_title_test(None);
    row[8] = Some("recording".to_owned());
    row[9] = Some("bogus".to_owned());
    executor
        .query_rows_result
        .insert(format!("{query_sql}|m1"), vec![row]);

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .get_meeting("m1")
        .expect_err("unknown stop_reason should fail");
    assert!(err.to_string().contains("invalid stop_reason"));
}

#[test]
fn sql_job_queue_parses_claimed_job_row() {
    let mut executor = FakeSqlExecutor::default();
    let claim_key = format!("{}|{}", CLAIM_JOB_SQL, "summarize");
    executor.query_rows_result.insert(
        claim_key,
        vec![sql_row_from_strings(vec![
            "j-1".to_owned(),
            "m-1".to_owned(),
            "summarize".to_owned(),
            "running".to_owned(),
            "2".to_owned(),
            "temporary error".to_owned(),
        ])],
    );

    let mut queue = SqlJobQueue::new(executor);
    let job = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(job.id, "j-1");
    assert_eq!(job.meeting_id, "m-1");
    assert_eq!(job.job_type, JobType::Summarize);
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(job.retry_count, 2);
    assert_eq!(job.error_message.as_deref(), Some("temporary error"));
}

#[test]
fn schema_defines_enum_check_constraints() {
    let schema = discord_transcript::infrastructure::sql::INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("meetings_status_check"));
    assert!(schema.contains("jobs_status_check"));
    assert!(schema.contains("jobs_job_type_check"));
    assert!(schema.contains("transcripts_source_check"));
    assert!(!discord_transcript::infrastructure::sql::INITIAL_SCHEMA_SQL
        .contains("status TEXT NOT NULL CHECK"));
    assert!(!discord_transcript::infrastructure::sql::INITIAL_SCHEMA_SQL
        .contains("job_type TEXT NOT NULL CHECK"));
    assert!(
        !discord_transcript::infrastructure::sql::INITIAL_SCHEMA_SQL
            .contains("transcripts_source_check")
    );
}

#[test]
fn sql_job_queue_retry_returns_failed_status() {
    let mut executor = FakeSqlExecutor::default();
    let retry_key = format!("{}|{}", RETRY_JOB_SQL, "j-1\u{1f}still failing\u{1f}1");
    executor
        .query_rows_result
        .insert(retry_key, vec![sql_row_from_strings(vec!["failed".to_owned()])]);
    let mut queue = SqlJobQueue::new(executor);

    let status = queue
        .retry("j-1", "still failing".to_owned(), 1)
        .expect("retry should succeed");
    assert_eq!(status, JobStatus::Failed);
}

#[test]
fn sql_store_set_status_with_cas_returns_not_found_when_meeting_missing() {
    let mut executor = FakeSqlExecutor::default();
    let cas_key = format!(
        "{}|{}",
        SET_MEETING_STATUS_CAS_SQL,
        "recording\u{1f}m-missing\u{1f}scheduled"
    );
    executor
        .query_rows_result
        .insert(cas_key, vec![sql_row_from_strings(vec!["not_found".to_owned()])]);

    let mut store = SqlMeetingStore::new(executor);
    let result = store.set_meeting_status(
        "m-missing",
        MeetingStatus::Recording,
        Some(MeetingStatus::Scheduled),
    );

    assert_eq!(
        result,
        Err(StoreError::NotFound {
            meeting_id: "m-missing".to_owned()
        })
    );
}

#[test]
fn sql_store_set_status_with_cas_returns_conflict_when_status_mismatch() {
    let mut executor = FakeSqlExecutor::default();
    let cas_key = format!(
        "{}|{}",
        SET_MEETING_STATUS_CAS_SQL,
        "recording\u{1f}m1\u{1f}scheduled"
    );
    executor
        .query_rows_result
        .insert(cas_key, vec![sql_row_from_strings(vec!["conflict".to_owned()])]);

    let mut store = SqlMeetingStore::new(executor);
    let result = store.set_meeting_status(
        "m1",
        MeetingStatus::Recording,
        Some(MeetingStatus::Scheduled),
    );

    assert_eq!(
        result,
        Err(StoreError::CasConflict {
            meeting_id: "m1".to_owned()
        })
    );
}

#[test]
fn sql_store_reads_and_sets_status_message_metadata() {
    let mut executor = FakeSqlExecutor::default();
    let query_sql = "SELECT report_channel_id, status_message_channel_id, status_message_id FROM meetings WHERE id=$1 LIMIT 1";
    executor.query_rows_result.insert(
        format!("{query_sql}|{}", "m1"),
        vec![sql_row_from_strings(vec![
            "c-report".to_owned(),
            "c-status".to_owned(),
            "m-status".to_owned(),
        ])],
    );

    let mut store = SqlMeetingStore::new(executor);
    let metadata = store
        .get_status_message_metadata("m1")
        .expect("metadata should load");
    assert_eq!(metadata.report_channel_id, "c-report");
    assert_eq!(
        metadata.status_message_channel_id.as_deref(),
        Some("c-status")
    );
    assert_eq!(metadata.status_message_id.as_deref(), Some("m-status"));

    store
        .set_status_message("m1", "c-new".to_owned(), "msg-2".to_owned())
        .expect("status message should persist");
    assert!(
        store.executor.executed.iter().any(|(sql, params)| {
            sql.contains("status_message_id")
                && params == &vec!["c-new".to_owned(), "msg-2".to_owned(), "m1".to_owned()]
        }),
        "set_status_message should execute update SQL"
    );
}

fn meeting_row_for_title_test(title: Option<String>) -> discord_transcript::infrastructure::sql_store::SqlRow {
    vec![
        Some("m1".to_owned()),
        Some("g1".to_owned()),
        Some("vc1".to_owned()),
        Some("c1".to_owned()),
        None,
        None,
        Some("u1".to_owned()),
        title,
        Some("recording".to_owned()),
        None,
        None,
        None,
        None,
    ]
}

#[test]
fn sql_store_get_meeting_distinguishes_null_title_from_empty_string() {
    let query_sql = "SELECT id, guild_id, voice_channel_id, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, \
                        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
                        to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as stopped_at \
                  FROM meetings WHERE id=$1 LIMIT 1";

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{query_sql}|m-null"),
        vec![meeting_row_for_title_test(None)],
    );
    let mut store = SqlMeetingStore::new(executor);
    let null_title = store
        .get_meeting("m-null")
        .expect("meeting should load")
        .expect("row should exist");
    assert_eq!(null_title.title, None);

    store.executor.query_rows_result.insert(
        format!("{query_sql}|m-empty"),
        vec![meeting_row_for_title_test(Some(String::new()))],
    );
    let empty_title = store
        .get_meeting("m-empty")
        .expect("meeting should load")
        .expect("row should exist");
    assert_eq!(empty_title.title.as_deref(), Some(""));
}
