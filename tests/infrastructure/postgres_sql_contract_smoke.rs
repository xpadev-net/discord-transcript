use discord_transcript::domain::{MeetingStatus, StopReason};
use discord_transcript::infrastructure::sql_store::{
    PgSqlExecutor, SqlExecutor, SqlMeetingStore,
};
use discord_transcript::infrastructure::storage::{
    CreateMeetingRequest, MeetingStore, StopTransition, StoreError,
};

#[test]
fn postgres_sql_contract_smoke_runs_migrations_and_core_queries() {
    let database_url = match std::env::var("DISCORD_TRANSCRIPT_TEST_DATABASE_URL") {
        Ok(database_url) => database_url,
        Err(_) if std::env::var_os("CI").is_some() => {
            panic!("DISCORD_TRANSCRIPT_TEST_DATABASE_URL is required in CI")
        }
        Err(_) => {
            eprintln!("skipping real Postgres SQL contract smoke test; set DISCORD_TRANSCRIPT_TEST_DATABASE_URL");
            return;
        }
    };

    let schema = format!("dt_sql_contract_{}", uuid::Uuid::new_v4().simple());
    eprintln!("running real Postgres SQL contract smoke test in isolated schema {schema}");
    let runtime = tokio::runtime::Runtime::new().expect("runtime should start");
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::connect(&database_url, tokio_postgres::NoTls)
            .await
            .expect("postgres test connection should open");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .await
            .expect("test schema should be created");
    });

    let result = run_sql_contract_smoke(&database_url, &schema);

    runtime.block_on(async {
        let (client, connection) = tokio_postgres::connect(&database_url, tokio_postgres::NoTls)
            .await
            .expect("postgres cleanup connection should open");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
            .await
            .expect("test schema should be dropped");
    });

    result.expect("Postgres SQL contract smoke should pass");
}

fn run_sql_contract_smoke(database_url: &str, schema: &str) -> Result<(), String> {
    let mut executor = PgSqlExecutor::connect(database_url)?;
    if let Some(threads) = postgres_executor_thread_count() {
        assert_eq!(threads, 1);
    }
    executor.run_migration(&format!("SET search_path TO {schema}"))?;
    let mut store = SqlMeetingStore::new(executor);
    store.apply_pending_migrations()?;

    store
        .create_meeting_as_recording(meeting_request("m-recording-1", "g-smoke", "vc-1"))
        .map_err(|err| err.to_string())?;

    let conflict =
        store.create_meeting_as_recording(meeting_request("m-recording-2", "g-smoke", "vc-2"));
    assert_active_meeting_conflict(conflict, "m-recording-1")?;

    let stop_transition = store
        .mark_stopping_if_recording("m-recording-1", StopReason::Manual)
        .map_err(|err| err.to_string())?;
    if stop_transition != StopTransition::Acquired {
        return Err(format!(
            "expected stop transition to acquire recording row, got {stop_transition:?}"
        ));
    }

    store
        .create_meeting_as_recording(meeting_request("m-recording-2", "g-smoke", "vc-2"))
        .map_err(|err| err.to_string())?;
    store
        .set_meeting_title("m-recording-2", "SQL contract smoke".to_owned())
        .map_err(|err| err.to_string())?;
    store
        .set_meeting_status(
            "m-recording-2",
            MeetingStatus::Transcribing,
            Some(MeetingStatus::Recording),
        )
        .map_err(|err| err.to_string())?;

    let cas_conflict = store.set_meeting_status(
        "m-recording-2",
        MeetingStatus::Summarizing,
        Some(MeetingStatus::Recording),
    );
    if cas_conflict
        != Err(StoreError::CasConflict {
            meeting_id: "m-recording-2".to_owned(),
        })
    {
        return Err(format!(
            "expected CAS conflict after status changed, got {cas_conflict:?}"
        ));
    }

    let rows = store.executor.query_rows(
        "SELECT status, title FROM meetings WHERE id=$1",
        &["m-recording-2".to_owned()],
    )?;
    let Some(row) = rows.first() else {
        return Err("expected m-recording-2 row to exist".to_owned());
    };
    if row.first().and_then(|value| value.as_deref()) != Some("transcribing") {
        return Err(format!("expected transcribing status, got {row:?}"));
    }
    if row.get(1).and_then(|value| value.as_deref()) != Some("SQL contract smoke") {
        return Err(format!("expected title update to persist, got {row:?}"));
    }

    for _ in 0..128 {
        let rows = store.executor.query_rows("SELECT 1", &[])?;
        if rows.first().and_then(|row| row.first()).and_then(Option::as_deref) != Some("1") {
            return Err(format!("unexpected repeated executor response: {rows:?}"));
        }
    }
    if let Some(threads) = postgres_executor_thread_count() {
        assert_eq!(threads, 1);
    }
    drop(store);
    if let Some(threads) = postgres_executor_thread_count() {
        assert_eq!(threads, 0);
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn postgres_executor_thread_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/task").ok().map(|entries| {
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_to_string(entry.path().join("comm")).ok())
            .filter(|name| name.trim() == "postgres-executor")
            .count()
    })
}

#[cfg(not(target_os = "linux"))]
fn postgres_executor_thread_count() -> Option<usize> {
    None
}

fn meeting_request(id: &str, guild_id: &str, voice_channel_id: &str) -> CreateMeetingRequest {
    CreateMeetingRequest {
        id: id.to_owned(),
        guild_id: guild_id.to_owned(),
        voice_channel_id: voice_channel_id.to_owned(),
        voice_channel_name: Some(format!("voice-{voice_channel_id}")),
        report_channel_id: "report-channel".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "user-smoke".to_owned(),
        effective_settings: None,
    }
}

fn assert_active_meeting_conflict(
    result: Result<(), StoreError>,
    expected_meeting_id: &str,
) -> Result<(), String> {
    match result {
        Err(StoreError::ActiveMeetingExists { meeting_id })
            if meeting_id == expected_meeting_id =>
        {
            Ok(())
        }
        other => Err(format!(
            "expected active meeting conflict for {expected_meeting_id}, got {other:?}"
        )),
    }
}
