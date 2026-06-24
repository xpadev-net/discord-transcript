use discord_transcript::audio::receiver::{BufferedFrame, ReceiverConfig, ReceiverState};
use discord_transcript::domain::MeetingStatus;
use discord_transcript::domain::StopReason;
use discord_transcript::domain::{JobStatus, JobType};
use discord_transcript::infrastructure::queue::{Job, JobQueue};
use discord_transcript::infrastructure::sql::{
    ACTIVE_MEETING_UNIQUE_INDEX_NAME, CLAIM_JOB_SQL, CREATE_SCHEMA_MIGRATIONS_SQL,
    FIND_ACTIVE_RECORDING_BLOCKER_BY_GUILD_SQL, INCREMENTAL_MIGRATIONS_SQL, INITIAL_SCHEMA_SQL,
    LOCK_SCHEMA_MIGRATIONS_SQL, MIGRATIONS, RETRY_JOB_SQL, SELECT_SCHEMA_MIGRATION_SQL,
    ROLLBACK_SCHEMA_MIGRATIONS_SQL, SET_MEETING_STATUS_CAS_SQL, UNLOCK_SCHEMA_MIGRATIONS_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlJobQueue, SqlMeetingStore, UNIQUE_VIOLATION_PREFIX, sql_row_from_strings,
    unique_violation_constraint,
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
            voice_channel_name: None,
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
fn sql_store_binds_captured_voice_channel_name_on_recording_create() {
    let executor = FakeSqlExecutor::default();
    let mut store = SqlMeetingStore::new(executor);

    store
        .create_meeting_as_recording(CreateMeetingRequest {
            id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc1".to_owned(),
            voice_channel_name: Some("Planning VC".to_owned()),
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
            effective_settings: None,
        })
        .expect("insert should execute");

    let (_, params) = store
        .executor
        .executed
        .last()
        .expect("insert should be recorded");
    assert_eq!(params[2], "vc1");
    assert_eq!(params[3], "Planning VC");
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
            voice_channel_name: None,
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
            duration_seconds: None,
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
    let query_sql = "SELECT id, guild_id, voice_channel_id, voice_channel_name, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, \
                        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
                        to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as stopped_at, \
                        meeting_duration_seconds \
                  FROM meetings WHERE id=$1 LIMIT 1";
    let mut corrupt_status_row = meeting_row_for_title_test(None);
    corrupt_status_row[9] = Some("corrupt".to_owned());
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
    let query_sql = "SELECT id, guild_id, voice_channel_id, voice_channel_name, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, \
                        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
                        to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as stopped_at, \
                        meeting_duration_seconds \
                  FROM meetings WHERE id=$1 LIMIT 1";
    let mut row = meeting_row_for_title_test(None);
    row[9] = Some("recording".to_owned());
    row[10] = Some("bogus".to_owned());
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
    let claim_key = format!("{}|*", CLAIM_JOB_SQL);
    executor.query_rows_result.insert(
        claim_key,
        vec![sql_row_from_strings(vec![
            "j-1".to_owned(),
            "m-1".to_owned(),
            "summarize".to_owned(),
            "running".to_owned(),
            "2".to_owned(),
            "temporary error".to_owned(),
            "token-1".to_owned(),
            "2026-06-08T01:02:33.000Z".to_owned(),
            "2026-06-08T01:02:03.000Z".to_owned(),
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
    assert_eq!(job.claim_token.as_deref(), Some("token-1"));
    assert!(job.leased_until.is_some());
    assert!(job.next_run_at.is_some());
}

#[test]
fn schema_defines_enum_check_constraints() {
    let schema = discord_transcript::infrastructure::sql::INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("meetings_status_check"));
    assert!(schema.contains("jobs_status_check"));
    assert!(schema.contains("'canceled'"));
    assert!(schema.contains("jobs_job_type_check"));
    assert!(schema.contains("next_run_at"));
    assert!(schema.contains("dead_lettered_at"));
    assert!(schema.contains("cancel_reason"));
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
fn active_meeting_unique_index_migration_is_registered() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("idx_meetings_one_active_blocking_per_guild"));
    assert!(schema.contains("WHERE status IN ('scheduled', 'recording')"));
    assert!(schema.contains("ranked_blocking_meetings"));
    assert!(schema.contains("CASE WHEN status = 'recording' THEN 0 ELSE 1 END"));
    assert!(schema.contains("WHEN ranked_blocking_meetings.status = 'recording'"));
    assert!(
        !schema.contains(
            "idx_meetings_one_active_blocking_per_guild\n    ON meetings (guild_id)\n    WHERE status IN ('scheduled', 'recording', 'stopping')"
        ),
        "stopping meetings must remain non-blocking for new recording starts"
    );
    assert!(
        MIGRATIONS
            .iter()
            .any(|migration| migration.version == "0028_active_meeting_unique_index")
    );
}

#[test]
fn schema_constrains_transcript_confidence_to_unit_range() {
    let initial_schema = INITIAL_SCHEMA_SQL;
    let incremental_schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(initial_schema.contains("transcripts_confidence_check"));
    assert!(initial_schema.contains("confidence >= 0.0 AND confidence <= 1.0"));
    assert!(incremental_schema.contains("transcripts_confidence_check"));
    assert!(incremental_schema.contains("confidence >= 0.0 AND confidence <= 1.0"));
    assert!(incremental_schema.contains("NOT VALID"));
    assert_eq!(
        MIGRATIONS.last().expect("latest migration").version,
        "0029_transcript_confidence_check"
    );
}

#[test]
fn unique_violation_constraint_requires_exact_prefixed_identity() {
    let error = format!(
        "{UNIQUE_VIOLATION_PREFIX}{ACTIVE_MEETING_UNIQUE_INDEX_NAME}: duplicate key value"
    );

    assert_eq!(
        unique_violation_constraint(&error),
        Some(ACTIVE_MEETING_UNIQUE_INDEX_NAME)
    );
    assert_eq!(
        unique_violation_constraint(
            "duplicate key value violates unique constraint idx_meetings_one_active_blocking_per_guild"
        ),
        None
    );
}

#[test]
fn scheduled_insert_unique_index_conflict_returns_active_blocker() {
    let mut executor = FakeSqlExecutor::default();
    let insert_error_key = format!(
        "INSERT INTO meetings(id,guild_id,voice_channel_id,voice_channel_name,report_channel_id,status_message_channel_id,status_message_id,started_by_user_id,status) VALUES($1,$2,$3,NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,'scheduled')|{}",
        "new\u{1f}g1\u{1f}vc-new\u{1f}\u{1f}c1\u{1f}\u{1f}\u{1f}u2"
    );
    executor.execute_error.insert(
        insert_error_key,
        format!("{UNIQUE_VIOLATION_PREFIX}{ACTIVE_MEETING_UNIQUE_INDEX_NAME}: duplicate key"),
    );
    executor.query_rows_result.insert(
        format!("{FIND_ACTIVE_RECORDING_BLOCKER_BY_GUILD_SQL}|g1"),
        vec![meeting_row("existing", "g1", "recording")],
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .create_scheduled_meeting(CreateMeetingRequest {
            id: "new".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc-new".to_owned(),
            voice_channel_name: None,
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u2".to_owned(),
            effective_settings: None,
        })
        .expect_err("active unique conflict should surface active blocker");

    assert_eq!(
        err,
        StoreError::ActiveMeetingExists {
            meeting_id: "existing".to_owned()
        }
    );
}

#[test]
fn recording_insert_unique_index_conflict_returns_active_blocker() {
    let mut executor = FakeSqlExecutor::default();
    let insert_error_key = format!(
        "INSERT INTO meetings(id,guild_id,voice_channel_id,voice_channel_name,report_channel_id,status_message_channel_id,status_message_id,started_by_user_id,status) VALUES($1,$2,$3,NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,'recording')|{}",
        "new\u{1f}g1\u{1f}vc-new\u{1f}\u{1f}c1\u{1f}\u{1f}\u{1f}u2"
    );
    executor.execute_error.insert(
        insert_error_key,
        format!("{UNIQUE_VIOLATION_PREFIX}{ACTIVE_MEETING_UNIQUE_INDEX_NAME}: duplicate key"),
    );
    executor.query_rows_result.insert(
        format!("{FIND_ACTIVE_RECORDING_BLOCKER_BY_GUILD_SQL}|g1"),
        vec![meeting_row("existing", "g1", "recording")],
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .create_meeting_as_recording(CreateMeetingRequest {
            id: "new".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc-new".to_owned(),
            voice_channel_name: None,
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u2".to_owned(),
            effective_settings: None,
        })
        .expect_err("active unique conflict should surface active blocker");

    assert_eq!(
        err,
        StoreError::ActiveMeetingExists {
            meeting_id: "existing".to_owned()
        }
    );
}

#[test]
fn recording_insert_ignores_stopping_row_when_recovering_active_conflict() {
    let mut executor = FakeSqlExecutor::default();
    let insert_error_key = format!(
        "INSERT INTO meetings(id,guild_id,voice_channel_id,voice_channel_name,report_channel_id,status_message_channel_id,status_message_id,started_by_user_id,status) VALUES($1,$2,$3,NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,'recording')|{}",
        "new\u{1f}g1\u{1f}vc-new\u{1f}\u{1f}c1\u{1f}\u{1f}\u{1f}u2"
    );
    executor.execute_error.insert(
        insert_error_key,
        format!("{UNIQUE_VIOLATION_PREFIX}{ACTIVE_MEETING_UNIQUE_INDEX_NAME}: duplicate key"),
    );
    executor
        .query_rows_result
        .insert(format!("{FIND_ACTIVE_RECORDING_BLOCKER_BY_GUILD_SQL}|g1"), vec![]);
    executor.active_by_guild.insert(
        "g1".to_owned(),
        StoredMeeting {
            id: "stopping-meeting".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc-old".to_owned(),
            voice_channel_name: None,
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
            title: None,
            status: MeetingStatus::Stopping,
            stop_reason: None,
            error_message: None,
            started_at: None,
            stopped_at: None,
            duration_seconds: None,
        },
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .create_meeting_as_recording(CreateMeetingRequest {
            id: "new".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc-new".to_owned(),
            voice_channel_name: None,
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u2".to_owned(),
            effective_settings: None,
        })
        .expect_err("missing blocker should return sanitized active conflict");

    assert_eq!(
        err,
        StoreError::ActiveMeetingExists {
            meeting_id: "new".to_owned()
        }
    );
}

#[test]
fn recording_insert_other_unique_violation_stays_duplicate_id_error() {
    let mut executor = FakeSqlExecutor::default();
    let insert_error_key = format!(
        "INSERT INTO meetings(id,guild_id,voice_channel_id,voice_channel_name,report_channel_id,status_message_channel_id,status_message_id,started_by_user_id,status) VALUES($1,$2,$3,NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,'recording')|{}",
        "new\u{1f}g1\u{1f}vc-new\u{1f}\u{1f}c1\u{1f}\u{1f}\u{1f}u2"
    );
    executor.execute_error.insert(
        insert_error_key,
        format!("{UNIQUE_VIOLATION_PREFIX}meetings_pkey: duplicate key"),
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .create_meeting_as_recording(CreateMeetingRequest {
            id: "new".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc-new".to_owned(),
            voice_channel_name: None,
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u2".to_owned(),
            effective_settings: None,
        })
        .expect_err("primary-key unique violation should stay duplicate id");

    assert_eq!(
        err,
        StoreError::AlreadyExists {
            meeting_id: "new".to_owned()
        }
    );
}

#[test]
fn pending_migrations_skip_versions_recorded_in_schema_migrations() {
    let mut executor = FakeSqlExecutor::default();
    for migration in MIGRATIONS {
        executor.query_rows_result.insert(
            format!("{SELECT_SCHEMA_MIGRATION_SQL}|{}", migration.version),
            vec![sql_row_from_strings(vec!["1".to_owned()])],
        );
    }

    let mut store = SqlMeetingStore::new(executor);
    store
        .apply_pending_migrations()
        .expect("migration check should succeed");

    assert_eq!(store.executor.executed.len(), MIGRATIONS.len() + 3);
    assert_eq!(store.executor.executed[0].0, LOCK_SCHEMA_MIGRATIONS_SQL);
    assert_eq!(store.executor.executed[1].0, CREATE_SCHEMA_MIGRATIONS_SQL);
    assert_eq!(
        store.executor.executed.last().expect("unlock").0,
        UNLOCK_SCHEMA_MIGRATIONS_SQL
    );
    assert!(
        store
            .executor
            .executed
            .iter()
            .skip(2)
            .take(MIGRATIONS.len())
            .all(|(sql, _)| *sql == SELECT_SCHEMA_MIGRATION_SQL)
    );
}

#[test]
fn pending_migrations_apply_and_record_unseen_versions() {
    let mut store = SqlMeetingStore::new(FakeSqlExecutor::default());

    store
        .apply_pending_migrations()
        .expect("migrations should apply");

    assert_eq!(store.executor.executed[0].0, LOCK_SCHEMA_MIGRATIONS_SQL);
    assert_eq!(store.executor.executed[1].0, CREATE_SCHEMA_MIGRATIONS_SQL);
    assert_eq!(
        store.executor.executed.last().expect("unlock").0,
        UNLOCK_SCHEMA_MIGRATIONS_SQL
    );
    let applied_sql: Vec<&str> = store
        .executor
        .executed
        .iter()
        .map(|(sql, _)| sql.as_str())
        .filter(|sql| sql.starts_with("BEGIN;"))
        .collect();
    assert_eq!(applied_sql.len(), MIGRATIONS.len());
    assert!(applied_sql[0].contains("CREATE TABLE IF NOT EXISTS meetings"));
    assert!(applied_sql[0].contains(
        "INSERT INTO schema_migrations (version) VALUES ('0001_mvp_schema')"
    ));
}

#[test]
fn pending_migrations_roll_back_before_unlock_after_migration_failure() {
    let failing_migration_sql = discord_transcript::infrastructure::sql::migration_transaction_sql(
        *MIGRATIONS.first().expect("migration should exist"),
    );
    let mut executor = FakeSqlExecutor::default();
    executor.run_migration_error.insert(
        failing_migration_sql.clone(),
        "migration exploded".to_owned(),
    );
    executor.run_migration_error.insert(
        ROLLBACK_SCHEMA_MIGRATIONS_SQL.to_owned(),
        "rollback cleanup failed".to_owned(),
    );
    executor.run_migration_error.insert(
        UNLOCK_SCHEMA_MIGRATIONS_SQL.to_owned(),
        "unlock cleanup failed".to_owned(),
    );
    let mut store = SqlMeetingStore::new(executor);

    let err = store
        .apply_pending_migrations()
        .expect_err("migration failure should be returned");

    assert_eq!(err, "migration exploded");
    let executed_sql: Vec<&str> = store
        .executor
        .executed
        .iter()
        .map(|(sql, _)| sql.as_str())
        .collect();
    assert_eq!(
        executed_sql,
        vec![
            LOCK_SCHEMA_MIGRATIONS_SQL,
            CREATE_SCHEMA_MIGRATIONS_SQL,
            SELECT_SCHEMA_MIGRATION_SQL,
            failing_migration_sql.as_str(),
            ROLLBACK_SCHEMA_MIGRATIONS_SQL,
            UNLOCK_SCHEMA_MIGRATIONS_SQL,
        ]
    );
}

#[test]
fn sql_job_queue_retry_returns_failed_status() {
    let mut executor = FakeSqlExecutor::default();
    let retry_key = format!(
        "{}|{}",
        RETRY_JOB_SQL, "j-1\u{1f}still failing\u{1f}1\u{1f}token-1"
    );
    executor
        .query_rows_result
        .insert(retry_key, vec![sql_row_from_strings(vec!["failed".to_owned()])]);
    let mut queue = SqlJobQueue::new(executor);
    let claimed = Job {
        id: "j-1".to_owned(),
        meeting_id: "m-1".to_owned(),
        job_type: JobType::Summarize,
        status: JobStatus::Running,
        retry_count: 0,
        error_message: None,
        claim_token: Some("token-1".to_owned()),
        leased_until: Some(chrono::Utc::now() + chrono::Duration::seconds(90)),
        next_run_at: None,
    };

    let status = queue
        .retry(&claimed, "still failing".to_owned(), 1)
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

fn meeting_row_for_title_test(
    title: Option<String>,
) -> discord_transcript::infrastructure::sql_store::SqlRow {
    vec![
        Some("m1".to_owned()),
        Some("g1".to_owned()),
        Some("vc1".to_owned()),
        None,
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
        None,
    ]
}

fn meeting_row(
    id: &str,
    guild_id: &str,
    status: &str,
) -> discord_transcript::infrastructure::sql_store::SqlRow {
    vec![
        Some(id.to_owned()),
        Some(guild_id.to_owned()),
        Some("vc1".to_owned()),
        None,
        Some("c1".to_owned()),
        None,
        None,
        Some("u1".to_owned()),
        None,
        Some(status.to_owned()),
        None,
        None,
        None,
        None,
        None,
    ]
}

#[test]
fn sql_store_get_meeting_distinguishes_null_title_from_empty_string() {
    let query_sql = "SELECT id, guild_id, voice_channel_id, voice_channel_name, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, \
                        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
                        to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as stopped_at, \
                        meeting_duration_seconds \
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
    assert_eq!(null_title.duration_seconds, None);

    let mut empty_title_row = meeting_row_for_title_test(Some(String::new()));
    empty_title_row[14] = Some("123".to_owned());
    store.executor.query_rows_result.insert(
        format!("{query_sql}|m-empty"),
        vec![empty_title_row],
    );
    let empty_title = store
        .get_meeting("m-empty")
        .expect("meeting should load")
        .expect("row should exist");
    assert_eq!(empty_title.title.as_deref(), Some(""));
    assert_eq!(empty_title.duration_seconds, Some(123));
}
