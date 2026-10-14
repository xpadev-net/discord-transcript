use discord_transcript::domain::{MeetingStatus, StopReason};
use discord_transcript::infrastructure::sql_store::{
    PgSqlExecutor, SqlExecutor, SqlMeetingStore,
};
use discord_transcript::infrastructure::sql::LIST_GUILD_MEETINGS_AFTER_CURSOR_SQL;
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

    store.executor.execute(
        "INSERT INTO meetings (id, guild_id, voice_channel_id, report_channel_id, started_by_user_id, status, started_at) SELECT 'page-' || n, CASE WHEN n % 10 = 0 THEN 'g-page-plan' ELSE 'g-other' END, 'vc-plan', 'report', 'user', 'posted', TIMESTAMPTZ '2025-01-01 00:00:00+00' - (n / 100) * INTERVAL '1 minute' FROM generate_series(1, 10000) AS n",
        &[],
    )
    .map_err(|err| format!("cursor fixture insert failed: {err}"))?;
    store
        .executor
        .run_migration("ANALYZE meetings")
        .map_err(|err| format!("cursor fixture analyze failed: {err}"))?;
    let first_page_sql = LIST_GUILD_MEETINGS_AFTER_CURSOR_SQL
        .replace("$1", "'g-page-plan'")
        .replace("$2::TEXT IS NULL", "TRUE")
        .replace("$3::TEXT IS NULL", "TRUE")
        .replace("$3::TEXT::TIMESTAMPTZ", "TIMESTAMPTZ '2025-01-01 00:00:00+00'")
        .replace("$4::TEXT", "'unused'")
        .replace("$5", "21");
    let first_page = store
        .executor
        .query_rows(&first_page_sql, &[])
        .map_err(|err| format!("first cursor page query failed: {err}"))?;
    if first_page.len() != 21 {
        return Err(format!("expected 21 rows in first cursor page, got {}", first_page.len()));
    }
    let last = first_page.last().expect("first page should contain rows");
    let cursor_time = last.get(3).and_then(Option::as_deref).unwrap();
    let cursor_id = last.first().and_then(Option::as_deref).unwrap();
    let second_page_sql = LIST_GUILD_MEETINGS_AFTER_CURSOR_SQL
        .replace("$1", "'g-page-plan'")
        .replace("$2::TEXT IS NULL", "TRUE")
        .replace("$3::TEXT IS NULL", "FALSE")
        .replace("$3::TEXT::TIMESTAMPTZ", &format!("TIMESTAMPTZ '{cursor_time}'"))
        .replace("$4::TEXT", &format!("'{cursor_id}'"))
        .replace("$5", "21");
    let second_page = store
        .executor
        .query_rows(&second_page_sql, &[])
        .map_err(|err| format!("second cursor page query failed: {err}"))?;
    let first_ids = first_page
        .iter()
        .filter_map(|row| row.first().and_then(Option::as_deref))
        .collect::<std::collections::HashSet<_>>();
    if second_page.iter().any(|row| {
        row.first()
            .and_then(Option::as_deref)
            .is_some_and(|id| first_ids.contains(id))
    }) {
        return Err("cursor pages must not repeat meeting IDs".to_owned());
    }
    let plan_sql = format!(
        "EXPLAIN (FORMAT TEXT) {}",
        LIST_GUILD_MEETINGS_AFTER_CURSOR_SQL
            .replace("$1", "'g-page-plan'")
            .replace("$2::TEXT IS NULL", "TRUE")
            .replace("$3::TEXT IS NULL", "FALSE")
            .replace("$3::TEXT::TIMESTAMPTZ", "TIMESTAMPTZ '2025-01-01 00:00:00+00'")
            .replace("$4::TEXT", "'page-100'")
            .replace("$5", "21"),
    );
    let plan = store
        .executor
        .query_rows(&plan_sql, &[])
        .map_err(|err| format!("cursor query EXPLAIN failed: {err}"))?;
    if plan.is_empty() || !plan.iter().any(|row| {
        row.first()
            .and_then(Option::as_deref)
            .is_some_and(|line| line.contains("idx_meetings_guild_started"))
    }) {
        return Err(format!("expected the guild/start index in cursor query plan, got {plan:?}"));
    }
    eprintln!("representative cursor query plan: {plan:?}");

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
            .filter(|name| name.trim() == "pg-sql-executor")
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
