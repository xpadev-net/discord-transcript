use discord_transcript::infrastructure::sql::{
    BACKFILL_DEFAULT_TENANTS_FROM_EXISTING_GUILDS_SQL, INCREMENTAL_MIGRATIONS_SQL,
    RESOLVE_TENANT_BY_GUILD_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlMeetingStore, sql_row_from_strings,
};

#[test]
fn incremental_migrations_include_tenant_installation_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS tenants"));
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS tenant_discord_guilds"));
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS tenant_memberships"));
    assert!(schema.contains("tenants_status_check"));
    assert!(schema.contains("tenant_discord_guilds_status_check"));
    assert!(schema.contains("idx_tenant_discord_guilds_active_guild"));
    assert!(schema.contains("idx_tenant_memberships_user_id"));
    assert!(schema.contains("WHERE status = 'active'"));
}

#[test]
fn tenant_installation_backfill_uses_existing_guild_ids_idempotently() {
    let sql = BACKFILL_DEFAULT_TENANTS_FROM_EXISTING_GUILDS_SQL;

    assert!(sql.contains("FROM meetings"));
    assert!(sql.contains("FROM guild_settings"));
    assert!(sql.contains("ON CONFLICT (id) DO NOTHING"));
    assert!(sql.contains("'migration:default-tenant:' || guild_id"));
    assert!(sql.contains("WHERE existing.guild_id = existing_guilds.guild_id"));
    assert!(!sql.contains("AND existing.status = 'active'"));
    assert!(!sql.contains("INSERT INTO tenant_memberships"));
}

#[test]
fn resolve_tenant_by_guild_requires_active_binding_and_tenant() {
    let sql = RESOLVE_TENANT_BY_GUILD_SQL;

    assert!(sql.contains("tg.guild_id = $1"));
    assert!(sql.contains("tg.status = 'active'"));
    assert!(sql.contains("t.status = 'active'"));
}

#[test]
fn sql_store_resolves_tenant_installation_by_guild() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{RESOLVE_TENANT_BY_GUILD_SQL}|g1"),
        vec![sql_row_from_strings(vec![
            "tenant-g1".to_owned(),
            "active".to_owned(),
            "2026-06-01T00:00:00Z".to_owned(),
            "g1".to_owned(),
            "migration".to_owned(),
        ])],
    );

    let mut store = SqlMeetingStore::new(executor);
    let resolved = store
        .resolve_tenant_by_guild("g1")
        .expect("tenant resolution should succeed")
        .expect("tenant should resolve");

    assert_eq!(resolved.tenant_id, "tenant-g1");
    assert_eq!(resolved.tenant_status, "active");
    assert_eq!(resolved.guild_id, "g1");
    assert_eq!(resolved.source, "migration");
    assert_eq!(
        resolved.period_anchor.expect("period anchor").to_rfc3339(),
        "2026-06-01T00:00:00+00:00"
    );
}

#[test]
fn sql_store_returns_none_when_guild_has_no_active_tenant() {
    let executor = FakeSqlExecutor::default();
    let mut store = SqlMeetingStore::new(executor);

    let resolved = store
        .resolve_tenant_by_guild("missing-guild")
        .expect("tenant resolution should not fail");

    assert_eq!(resolved, None);
}

#[test]
fn sql_store_rejects_malformed_tenant_resolution_rows() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{RESOLVE_TENANT_BY_GUILD_SQL}|g1"),
        vec![sql_row_from_strings(vec!["tenant-g1".to_owned()])],
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .resolve_tenant_by_guild("g1")
        .expect_err("short row should be rejected");

    assert!(err.to_string().contains("tenant installation row length"));
}

#[test]
fn sql_store_rejects_invalid_tenant_period_anchor() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{RESOLVE_TENANT_BY_GUILD_SQL}|g1"),
        vec![sql_row_from_strings(vec![
            "tenant-g1".to_owned(),
            "active".to_owned(),
            "not-a-timestamp".to_owned(),
            "g1".to_owned(),
            "migration".to_owned(),
        ])],
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .resolve_tenant_by_guild("g1")
        .expect_err("invalid period_anchor should be rejected");

    assert!(err.to_string().contains("invalid tenant period_anchor"));
}

#[test]
fn sql_store_propagates_tenant_resolution_backend_errors() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_error.insert(
        format!("{RESOLVE_TENANT_BY_GUILD_SQL}|g1"),
        "database unavailable".to_owned(),
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .resolve_tenant_by_guild("g1")
        .expect_err("backend error should be propagated");

    assert!(err.to_string().contains("database unavailable"));
}

#[test]
fn sql_store_parses_default_tenant_backfill_counts() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{BACKFILL_DEFAULT_TENANTS_FROM_EXISTING_GUILDS_SQL}|"),
        vec![sql_row_from_strings(vec!["2".to_owned(), "3".to_owned()])],
    );

    let mut store = SqlMeetingStore::new(executor);
    let counts = store
        .backfill_default_tenants_from_existing_guilds()
        .expect("backfill should parse counts");

    assert_eq!(counts.tenants_inserted, 2);
    assert_eq!(counts.installations_inserted, 3);
}
