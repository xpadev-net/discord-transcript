use discord_transcript::infrastructure::sql_store::{
    PgSqlExecutor, SqlExecutor, SqlMeetingStore,
};
use discord_transcript::infrastructure::storage::{
    CreateMeetingRequest, MeetingStore, StoreError,
};
use std::sync::{Arc, Barrier};

#[test]
fn postgres_allows_only_one_concurrent_recording_per_guild() {
    let Ok(database_url) = std::env::var("DISCORD_TRANSCRIPT_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping real Postgres active-meeting concurrency test; set DISCORD_TRANSCRIPT_TEST_DATABASE_URL"
        );
        return;
    };

    let schema = format!("dt_active_meeting_{}", uuid::Uuid::new_v4().simple());
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

    let result = run_concurrent_recording_insert_test(&database_url, &schema);

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

    result.expect("concurrent insert regression should pass");
}

fn run_concurrent_recording_insert_test(database_url: &str, schema: &str) -> Result<(), String> {
    let mut migrator = PgSqlExecutor::connect(database_url)?;
    migrator.run_migration(&format!("SET search_path TO {schema}"))?;
    let mut migration_store = SqlMeetingStore::new(migrator);
    migration_store.apply_pending_migrations()?;

    let barrier = Arc::new(Barrier::new(2));
    let handles = ["m1", "m2"].map(|meeting_id| {
        let database_url = database_url.to_owned();
        let schema = schema.to_owned();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let mut executor = PgSqlExecutor::connect(&database_url).map_err(StoreError::Backend)?;
            executor
                .run_migration(&format!("SET search_path TO {schema}"))
                .map_err(StoreError::Backend)?;
            let mut store = SqlMeetingStore::new(executor);
            barrier.wait();
            store.create_meeting_as_recording(CreateMeetingRequest {
                id: meeting_id.to_owned(),
                guild_id: "g1".to_owned(),
                voice_channel_id: format!("vc-{meeting_id}"),
                voice_channel_name: None,
                report_channel_id: "c1".to_owned(),
                status_message_channel_id: None,
                status_message_id: None,
                started_by_user_id: format!("u-{meeting_id}"),
                effective_settings: None,
            })
        })
    });

    let mut results = Vec::new();
    for handle in handles {
        results.push(
            handle
                .join()
                .map_err(|_| "insert thread panicked".to_owned())?,
        );
    }

    let successes: Vec<_> = results.iter().filter(|result| result.is_ok()).collect();
    let active_conflicts: Vec<_> = results
        .iter()
        .filter(|result| matches!(result, Err(StoreError::ActiveMeetingExists { .. })))
        .collect();
    if successes.len() != 1 || active_conflicts.len() != 1 {
        return Err(format!(
            "expected one insert success and one active conflict, got {results:?}"
        ));
    }

    let mut verifier = PgSqlExecutor::connect(database_url)?;
    verifier.run_migration(&format!("SET search_path TO {schema}"))?;
    let rows = verifier.query_rows(
        "SELECT COUNT(*)::BIGINT FROM meetings WHERE guild_id=$1 AND status='recording'",
        &["g1".to_owned()],
    )?;
    let recording_count = rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.as_deref())
        .ok_or_else(|| "recording count row missing".to_owned())?;
    if recording_count != "1" {
        return Err(format!("expected one recording row, got {recording_count}"));
    }

    Ok(())
}
