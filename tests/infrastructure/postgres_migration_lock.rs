use discord_transcript::infrastructure::sql::{
    CREATE_SCHEMA_MIGRATIONS_SQL, MIGRATIONS, UNLOCK_SCHEMA_MIGRATIONS_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    PgSqlExecutor, SqlExecutor, SqlMeetingStore,
};

#[test]
fn failed_postgres_migration_does_not_leave_advisory_lock_stuck() {
    let Ok(database_url) = std::env::var("DISCORD_TRANSCRIPT_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping real Postgres migration-lock smoke test; set DISCORD_TRANSCRIPT_TEST_DATABASE_URL"
        );
        return;
    };

    let schema = format!("dt_migration_lock_{}", uuid::Uuid::new_v4().simple());
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

    let result = run_failed_migration_lock_test(&database_url, &schema);

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

    result.expect("failed migration should clean up the advisory lock");
}

fn run_failed_migration_lock_test(database_url: &str, schema: &str) -> Result<(), String> {
    let mut migrator = PgSqlExecutor::connect(database_url)?;
    migrator.run_migration(&format!("SET search_path TO {schema}"))?;
    migrator.run_migration(CREATE_SCHEMA_MIGRATIONS_SQL)?;
    for migration in MIGRATIONS {
        if migration.version == "0029_transcript_confidence_check" {
            continue;
        }
        migrator.execute(
            "INSERT INTO schema_migrations (version) VALUES ($1)",
            &[migration.version.to_owned()],
        )?;
    }
    let mut store = SqlMeetingStore::new(migrator);

    let err = store
        .apply_pending_migrations()
        .expect_err("latest migration should fail against the intentionally incomplete schema");
    if !err.contains("transcripts") {
        return Err(format!("expected transcripts migration error, got {err}"));
    }

    let mut verifier = PgSqlExecutor::connect(database_url)?;
    let rows = verifier.query_rows("SELECT pg_try_advisory_lock(760918997406360681)", &[])?;
    let lock_available = rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.as_deref())
        == Some("true");
    if lock_available {
        verifier.run_migration(UNLOCK_SCHEMA_MIGRATIONS_SQL)?;
        Ok(())
    } else {
        Err(format!("migration advisory lock remained held after error: {err}"))
    }
}
