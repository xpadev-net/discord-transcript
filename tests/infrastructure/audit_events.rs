use chrono::{TimeZone, Utc};
use discord_transcript::domain::audit::AuditEvent;
use discord_transcript::infrastructure::sql::{
    INCREMENTAL_MIGRATIONS_SQL, INSERT_AUDIT_EVENT_SQL, LIST_RECENT_AUDIT_EVENTS_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlMeetingStore, audit_event_params, sql_row_from_strings,
};

fn audit_event() -> AuditEvent {
    let occurred_at = Utc.with_ymd_and_hms(2026, 6, 3, 1, 2, 3).unwrap();
    AuditEvent {
        id: "audit-1".to_owned(),
        tenant_id: Some("tenant-g1".to_owned()),
        guild_id: Some("g1".to_owned()),
        actor_user_id: Some("u1".to_owned()),
        action: "guild_settings.update".to_owned(),
        resource_type: "guild_settings".to_owned(),
        resource_id: Some("g1".to_owned()),
        request_metadata_json: r#"{"method":"PUT","path":"/api/guild/settings"}"#.to_owned(),
        detail_json: r#"{"summary_enabled":true}"#.to_owned(),
        occurred_at,
        created_at: occurred_at,
    }
}

#[test]
fn incremental_migrations_include_audit_event_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS audit_events"));
    assert!(schema.contains("request_metadata JSONB NOT NULL"));
    assert!(schema.contains("detail_json JSONB NOT NULL"));
    assert!(schema.contains("idx_audit_events_guild_recent"));
    assert!(schema.contains("idx_audit_events_actor_recent"));
}

#[test]
fn audit_insert_sql_prunes_old_rows_and_dedupes_debug_downloads() {
    assert!(INSERT_AUDIT_EVENT_SQL.contains("stale_audit_events"));
    assert!(INSERT_AUDIT_EVENT_SQL.contains("audit_retention_sample"));
    assert!(INSERT_AUDIT_EVENT_SQL.contains("random() < 0.001"));
    assert!(INSERT_AUDIT_EVENT_SQL.contains("INTERVAL '180 days'"));
    assert!(INSERT_AUDIT_EVENT_SQL.contains("LIMIT 500"));
    assert!(INSERT_AUDIT_EVENT_SQL.contains("ON CONFLICT (id) DO NOTHING"));
    assert!(!INSERT_AUDIT_EVENT_SQL.contains("INTERVAL '15 minutes'"));
}

#[test]
fn audit_event_params_omit_missing_optional_fields_as_empty_strings() {
    let mut event = audit_event();
    event.tenant_id = None;
    event.actor_user_id = None;
    event.resource_id = None;

    let params = audit_event_params(&event);

    assert_eq!(params[0], "audit-1");
    assert_eq!(params[1], "");
    assert_eq!(params[2], "g1");
    assert_eq!(params[3], "");
    assert_eq!(params[6], "");
    assert_eq!(params[9], "2026-06-03T01:02:03+00:00");
}

#[test]
fn sql_store_appends_and_lists_recent_audit_events() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{LIST_RECENT_AUDIT_EVENTS_SQL}|tenant-g1\u{1f}g1\u{1f}20"),
        vec![sql_row_from_strings(vec![
            "audit-1".to_owned(),
            "tenant-g1".to_owned(),
            "g1".to_owned(),
            "u1".to_owned(),
            "guild_settings.update".to_owned(),
            "guild_settings".to_owned(),
            "g1".to_owned(),
            r#"{"method":"PUT","path":"/api/guild/settings"}"#.to_owned(),
            r#"{"summary_enabled":true}"#.to_owned(),
            "2026-06-03T01:02:03.000Z".to_owned(),
            "2026-06-03T01:02:04.000Z".to_owned(),
        ])],
    );
    let mut store = SqlMeetingStore::new(executor);

    store
        .append_audit_event(&audit_event())
        .expect("append should execute");
    let events = store
        .list_recent_audit_events(Some("tenant-g1"), Some("g1"), 20)
        .expect("recent audit events should parse");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "guild_settings.update");
    assert_eq!(events[0].resource_type, "guild_settings");
    assert_eq!(events[0].guild_id.as_deref(), Some("g1"));
    assert_eq!(
        events[0].occurred_at.to_rfc3339(),
        "2026-06-03T01:02:03+00:00"
    );
    assert!(
        store
            .executor
            .executed
            .iter()
            .any(|(sql, _)| sql == INSERT_AUDIT_EVENT_SQL)
    );
}

#[test]
fn sql_store_rejects_malformed_audit_rows() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{LIST_RECENT_AUDIT_EVENTS_SQL}|\u{1f}g1\u{1f}5"),
        vec![sql_row_from_strings(vec![
            "audit-1".to_owned(),
            "tenant-g1".to_owned(),
            "g1".to_owned(),
            "u1".to_owned(),
            "guild_settings.update".to_owned(),
            "guild_settings".to_owned(),
            "g1".to_owned(),
            "{}".to_owned(),
            "{}".to_owned(),
            "not-a-time".to_owned(),
            "2026-06-03T01:02:04.000Z".to_owned(),
        ])],
    );
    let mut store = SqlMeetingStore::new(executor);

    let err = store
        .list_recent_audit_events(None, Some("g1"), 5)
        .expect_err("invalid timestamp should be rejected");

    assert!(err.to_string().contains("invalid audit occurred_at"));
}

#[test]
fn sql_store_clamps_recent_audit_limit_before_querying() {
    let executor = FakeSqlExecutor::default();
    let mut store = SqlMeetingStore::new(executor);

    let events = store
        .list_recent_audit_events(None, Some("g1"), u32::MAX)
        .expect("clamped limit should be safe to query");

    assert!(events.is_empty());
    assert_eq!(
        store.executor.executed[0].1,
        vec!["".to_owned(), "g1".to_owned(), "100".to_owned()]
    );
}

#[test]
fn sql_store_preserves_zero_recent_audit_limit() {
    let executor = FakeSqlExecutor::default();
    let mut store = SqlMeetingStore::new(executor);

    let events = store
        .list_recent_audit_events(None, Some("g1"), 0)
        .expect("zero limit should be safe to query");

    assert!(events.is_empty());
    assert_eq!(
        store.executor.executed[0].1,
        vec!["".to_owned(), "g1".to_owned(), "0".to_owned()]
    );
}
