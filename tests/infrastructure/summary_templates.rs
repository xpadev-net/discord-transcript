use discord_transcript::domain::summary_template::{NewSummaryTemplate, UpdateSummaryTemplate};
use discord_transcript::infrastructure::sql::{
    ACTIVATE_SUMMARY_TEMPLATE_SQL, ARCHIVE_SUMMARY_TEMPLATE_SQL, GET_ACTIVE_SUMMARY_TEMPLATE_SQL,
    INCREMENTAL_MIGRATIONS_SQL, INSERT_SUMMARY_TEMPLATE_SQL, LIST_SUMMARY_TEMPLATES_SQL,
    UPDATE_SUMMARY_TEMPLATE_SQL,
};
use discord_transcript::infrastructure::sql_store::{FakeSqlExecutor, SqlMeetingStore, SqlRow};

fn summary_template_row(
    id: &str,
    tenant_id: Option<&str>,
    guild_id: &str,
    active: bool,
    version: u32,
    archived_at: Option<&str>,
) -> SqlRow {
    vec![
        Some(id.to_owned()),
        tenant_id.map(str::to_owned),
        Some(guild_id.to_owned()),
        Some("Default summary".to_owned()),
        Some("Read {{transcript_path}} and {{manifest_path}}.".to_owned()),
        Some(active.to_string()),
        Some(version.to_string()),
        Some("actor-1".to_owned()),
        archived_at.map(str::to_owned),
        archived_at.map(|_| "actor-2".to_owned()),
        Some("2026-06-03T01:02:03.000Z".to_owned()),
        Some("2026-06-03T01:02:04.000Z".to_owned()),
    ]
}

#[test]
fn incremental_migrations_include_summary_template_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS summary_templates"));
    assert!(schema.contains("tenant_id TEXT NOT NULL REFERENCES tenants(id)"));
    assert!(schema.contains("guild_id TEXT NOT NULL"));
    assert!(schema.contains("active BOOLEAN NOT NULL DEFAULT TRUE"));
    assert!(schema.contains("version INTEGER NOT NULL DEFAULT 1"));
    assert!(schema.contains("archived_at TIMESTAMPTZ"));
    assert!(schema.contains("idx_summary_templates_tenant_guild_active"));
    assert!(schema.contains("idx_summary_templates_one_active_per_guild"));
    assert!(schema.contains("WHERE active = TRUE AND archived_at IS NULL"));
}

#[test]
fn summary_template_sql_scopes_to_active_tenant_and_authenticated_guild() {
    assert!(LIST_SUMMARY_TEMPLATES_SQL.contains("WHERE guild_id = $1"));
    assert!(LIST_SUMMARY_TEMPLATES_SQL.contains("tg.status = 'active'"));
    assert!(LIST_SUMMARY_TEMPLATES_SQL.contains("t.status = 'active'"));
    assert!(LIST_SUMMARY_TEMPLATES_SQL.contains("tenant_id = (SELECT tenant_id FROM active_tenant)"));
    assert!(LIST_SUMMARY_TEMPLATES_SQL.contains("archived_at IS NULL"));
    assert!(GET_ACTIVE_SUMMARY_TEMPLATE_SQL.contains("active = TRUE"));
    assert!(GET_ACTIVE_SUMMARY_TEMPLATE_SQL.contains("archived_at IS NULL"));
    assert!(INSERT_SUMMARY_TEMPLATE_SQL.contains("FROM active_tenant"));
}

#[test]
fn summary_template_mutations_increment_version_and_manage_active_archive_state() {
    assert!(INSERT_SUMMARY_TEMPLATE_SQL.contains("deactivate_others"));
    assert!(INSERT_SUMMARY_TEMPLATE_SQL.contains("WHERE $5::TEXT::BOOLEAN"));
    assert!(UPDATE_SUMMARY_TEMPLATE_SQL.contains("deactivate_others"));
    assert!(UPDATE_SUMMARY_TEMPLATE_SQL.contains("THEN version + 1"));
    assert!(UPDATE_SUMMARY_TEMPLATE_SQL.contains("AND archived_at IS NULL"));
    assert!(ACTIVATE_SUMMARY_TEMPLATE_SQL.contains("deactivate_others"));
    assert!(ACTIVATE_SUMMARY_TEMPLATE_SQL.contains("active = TRUE"));
    assert!(ACTIVATE_SUMMARY_TEMPLATE_SQL.contains("archived_at = NULL"));
    assert!(ARCHIVE_SUMMARY_TEMPLATE_SQL.contains("WHEN archived_at IS NULL THEN FALSE"));
    assert!(ARCHIVE_SUMMARY_TEMPLATE_SQL.contains("archived_at = COALESCE(archived_at, NOW())"));
}

#[test]
fn sql_store_lists_summary_templates_with_filters_and_parses_rows() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{LIST_SUMMARY_TEMPLATES_SQL}|g1\u{1f}false"),
        vec![summary_template_row("st-1", Some("tenant-g1"), "g1", true, 3, None)],
    );
    let mut store = SqlMeetingStore::new(executor);

    let templates = store
        .list_summary_templates("g1", false)
        .expect("summary template rows should parse");

    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].id, "st-1");
    assert_eq!(templates[0].tenant_id.as_deref(), Some("tenant-g1"));
    assert_eq!(templates[0].guild_id, "g1");
    assert_eq!(templates[0].version, 3);
    assert!(templates[0].active);
    assert!(templates[0].archived_at.is_none());
    assert_eq!(
        store.executor.executed[0].1,
        vec!["g1".to_owned(), "false".to_owned()]
    );
}

#[test]
fn sql_store_gets_active_summary_template() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{GET_ACTIVE_SUMMARY_TEMPLATE_SQL}|g1"),
        vec![summary_template_row("st-1", Some("tenant-g1"), "g1", true, 2, None)],
    );
    let mut store = SqlMeetingStore::new(executor);

    let template = store
        .get_active_summary_template("g1")
        .expect("active summary template should parse")
        .expect("active template should exist");

    assert_eq!(template.id, "st-1");
    assert!(template.active);
}

#[test]
fn sql_store_creates_updates_activates_and_archives_summary_templates() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!(
            "{INSERT_SUMMARY_TEMPLATE_SQL}|st-1\u{1f}g1\u{1f}Default summary\u{1f}Read {{{{transcript_path}}}}.\u{1f}true\u{1f}actor-1"
        ),
        vec![summary_template_row("st-1", Some("tenant-g1"), "g1", true, 1, None)],
    );
    executor.query_rows_result.insert(
        format!(
            "{UPDATE_SUMMARY_TEMPLATE_SQL}|st-1\u{1f}g1\u{1f}Updated summary\u{1f}Read {{{{manifest_path}}}}.\u{1f}false\u{1f}actor-2"
        ),
        vec![summary_template_row("st-1", Some("tenant-g1"), "g1", false, 2, None)],
    );
    executor.query_rows_result.insert(
        format!("{ACTIVATE_SUMMARY_TEMPLATE_SQL}|st-1\u{1f}g1\u{1f}actor-3"),
        vec![summary_template_row("st-1", Some("tenant-g1"), "g1", true, 3, None)],
    );
    executor.query_rows_result.insert(
        format!("{ARCHIVE_SUMMARY_TEMPLATE_SQL}|st-1\u{1f}g1\u{1f}actor-4"),
        vec![summary_template_row(
            "st-1",
            Some("tenant-g1"),
            "g1",
            false,
            4,
            Some("2026-06-03T02:00:00.000Z"),
        )],
    );
    let mut store = SqlMeetingStore::new(executor);

    let created = store
        .create_summary_template(&NewSummaryTemplate {
            id: "st-1".to_owned(),
            guild_id: "g1".to_owned(),
            name: "Default summary".to_owned(),
            template: "Read {{transcript_path}}.".to_owned(),
            active: true,
            updated_actor_user_id: Some("actor-1".to_owned()),
        })
        .expect("create should parse returned row");
    assert_eq!(created.version, 1);

    let updated = store
        .update_summary_template(&UpdateSummaryTemplate {
            id: "st-1".to_owned(),
            guild_id: "g1".to_owned(),
            name: "Updated summary".to_owned(),
            template: "Read {{manifest_path}}.".to_owned(),
            active: Some(false),
            updated_actor_user_id: Some("actor-2".to_owned()),
        })
        .expect("update should parse")
        .expect("row should exist");
    assert_eq!(updated.version, 2);
    assert!(!updated.active);

    let activated = store
        .activate_summary_template("g1", "st-1", Some("actor-3"))
        .expect("activate should parse")
        .expect("row should exist");
    assert!(activated.active);
    assert_eq!(activated.version, 3);

    let archived = store
        .archive_summary_template("g1", "st-1", "actor-4")
        .expect("archive should parse")
        .expect("row should exist");
    assert!(!archived.active);
    assert_eq!(archived.version, 4);
    assert!(archived.archived_at.is_some());
    assert_eq!(archived.archived_actor_user_id.as_deref(), Some("actor-2"));
}

#[test]
fn sql_store_requires_summary_template_archive_actor_before_querying() {
    let executor = FakeSqlExecutor::default();
    let mut store = SqlMeetingStore::new(executor);

    let err = store
        .archive_summary_template("g1", "st-1", "")
        .expect_err("blank archive actor should be rejected");

    assert!(err.to_string().contains("archive actor is required"));
    assert!(store.executor.executed.is_empty());
}

#[test]
fn sql_store_rejects_invalid_summary_template_rows() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{LIST_SUMMARY_TEMPLATES_SQL}|g1\u{1f}true"),
        vec![vec![Some("too-short".to_owned())]],
    );
    let mut store = SqlMeetingStore::new(executor);

    let err = store
        .list_summary_templates("g1", true)
        .expect_err("invalid rows should be rejected");

    assert!(err.to_string().contains("invalid summary template row length"));
}
