use chrono::{TimeZone, Utc};
use discord_transcript::domain::usage::{NewUsageEvent, UsageMetric};
use discord_transcript::infrastructure::sql::{
    AGGREGATE_RECENT_USAGE_SQL, INCREMENTAL_MIGRATIONS_SQL, INSERT_USAGE_EVENT_SQL,
    LIST_RECENT_USAGE_EVENTS_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlMeetingStore, sql_row_from_strings, usage_event_params,
};

fn usage_event() -> NewUsageEvent {
    NewUsageEvent {
        id: "usage-1".to_owned(),
        tenant_id: Some("tenant-g1".to_owned()),
        guild_id: "g1".to_owned(),
        meeting_id: Some("m1".to_owned()),
        job_id: Some("j1".to_owned()),
        resource_type: Some("meeting".to_owned()),
        resource_id: Some("m1".to_owned()),
        metric: UsageMetric::AsrSeconds,
        quantity: 42,
        detail_json: r#"{"source":"test"}"#.to_owned(),
        observed_at: Utc.with_ymd_and_hms(2026, 6, 3, 1, 2, 3).unwrap(),
    }
}

#[test]
fn incremental_migrations_include_usage_event_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS usage_events"));
    assert!(schema.contains("metric IN"));
    assert!(schema.contains("'recording_minutes'"));
    assert!(schema.contains("'asr_seconds'"));
    assert!(schema.contains("'summary_runs'"));
    assert!(schema.contains("'storage_bytes'"));
    assert!(schema.contains("'debug_downloads'"));
    assert!(schema.contains("usage_events_guild_id_nonempty_check"));
    assert!(schema.contains("idx_usage_events_guild_recent"));
}

#[test]
fn usage_event_params_omit_missing_optional_fields_as_empty_strings() {
    let mut event = usage_event();
    event.tenant_id = None;
    event.job_id = None;
    event.resource_id = None;

    let params = usage_event_params(&event);

    assert_eq!(params[0], "usage-1");
    assert_eq!(params[1], "");
    assert_eq!(params[2], "g1");
    assert_eq!(params[4], "");
    assert_eq!(params[6], "");
    assert_eq!(params[7], "asr_seconds");
    assert_eq!(params[8], "42");
}

#[test]
fn sql_store_appends_lists_and_aggregates_usage_events() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{LIST_RECENT_USAGE_EVENTS_SQL}|tenant-g1\u{1f}g1\u{1f}20"),
        vec![sql_row_from_strings(vec![
            "usage-1".to_owned(),
            "tenant-g1".to_owned(),
            "g1".to_owned(),
            "m1".to_owned(),
            "j1".to_owned(),
            "meeting".to_owned(),
            "m1".to_owned(),
            "asr_seconds".to_owned(),
            "42".to_owned(),
            r#"{"source":"test"}"#.to_owned(),
            "2026-06-03T01:02:03.000Z".to_owned(),
            "2026-06-03T01:02:04.000Z".to_owned(),
        ])],
    );
    executor.query_rows_result.insert(
        format!("{AGGREGATE_RECENT_USAGE_SQL}|tenant-g1\u{1f}g1\u{1f}86400"),
        vec![sql_row_from_strings(vec![
            "summary_runs".to_owned(),
            "3".to_owned(),
        ])],
    );
    let mut store = SqlMeetingStore::new(executor);

    store
        .append_usage_event(&usage_event())
        .expect("append should execute");
    let events = store
        .list_recent_usage_events(Some("tenant-g1"), Some("g1"), 20)
        .expect("usage events should parse");
    let aggregates = store
        .aggregate_recent_usage(Some("tenant-g1"), Some("g1"), 86_400)
        .expect("usage aggregates should parse");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].metric, UsageMetric::AsrSeconds);
    assert_eq!(events[0].quantity, 42);
    assert_eq!(aggregates[0].metric, UsageMetric::SummaryRuns);
    assert_eq!(aggregates[0].quantity, 3);
    assert!(INSERT_USAGE_EVENT_SQL.contains("tenant_discord_guilds"));
    assert!(LIST_RECENT_USAGE_EVENTS_SQL.contains("tenant_discord_guilds"));
    assert!(AGGREGATE_RECENT_USAGE_SQL.contains("tenant_discord_guilds"));
    assert!(
        store
            .executor
            .executed
            .iter()
            .any(|(sql, _)| sql == INSERT_USAGE_EVENT_SQL && sql.contains("ON CONFLICT (id) DO NOTHING"))
    );
}

#[test]
fn sql_store_rejects_malformed_usage_rows() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{LIST_RECENT_USAGE_EVENTS_SQL}|\u{1f}g1\u{1f}5"),
        vec![sql_row_from_strings(vec![
            "usage-1".to_owned(),
            "tenant-g1".to_owned(),
            "g1".to_owned(),
            "m1".to_owned(),
            "j1".to_owned(),
            "meeting".to_owned(),
            "m1".to_owned(),
            "not_a_metric".to_owned(),
            "42".to_owned(),
            "{}".to_owned(),
            "2026-06-03T01:02:03.000Z".to_owned(),
            "2026-06-03T01:02:04.000Z".to_owned(),
        ])],
    );
    let mut store = SqlMeetingStore::new(executor);

    let err = store
        .list_recent_usage_events(None, Some("g1"), 5)
        .expect_err("invalid metric should be rejected");

    assert!(err.to_string().contains("unknown usage metric"));
}
