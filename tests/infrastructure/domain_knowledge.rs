use discord_transcript::domain::domain_knowledge::{
    DomainKnowledgeContentType, NewDomainKnowledgeItem, UpdateDomainKnowledgeItem,
};
use discord_transcript::infrastructure::sql::{
    ACTIVATE_DOMAIN_KNOWLEDGE_SQL, ARCHIVE_DOMAIN_KNOWLEDGE_SQL, INCREMENTAL_MIGRATIONS_SQL,
    INSERT_DOMAIN_KNOWLEDGE_SQL, LIST_DOMAIN_KNOWLEDGE_SQL, UPDATE_DOMAIN_KNOWLEDGE_SQL,
};
use discord_transcript::infrastructure::sql_store::{FakeSqlExecutor, SqlMeetingStore, SqlRow};

fn domain_row(
    id: &str,
    tenant_id: Option<&str>,
    guild_id: &str,
    content_type: &str,
    active: bool,
    version: u32,
    archived_at: Option<&str>,
) -> SqlRow {
    vec![
        Some(id.to_owned()),
        tenant_id.map(str::to_owned),
        Some(guild_id.to_owned()),
        Some(content_type.to_owned()),
        Some("Release names".to_owned()),
        Some("Use the public project codename.".to_owned()),
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
fn incremental_migrations_include_domain_knowledge_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS domain_knowledge_items"));
    assert!(schema.contains("tenant_id TEXT NOT NULL REFERENCES tenants(id)"));
    assert!(schema.contains("guild_id TEXT NOT NULL"));
    assert!(schema.contains("active BOOLEAN NOT NULL DEFAULT TRUE"));
    assert!(schema.contains("version INTEGER NOT NULL DEFAULT 1"));
    assert!(schema.contains("archived_at TIMESTAMPTZ"));
    assert!(schema.contains("idx_domain_knowledge_tenant_guild_active"));
}

#[test]
fn domain_knowledge_content_types_are_limited_to_known_values() {
    assert_eq!(
        DomainKnowledgeContentType::parse_str("glossary"),
        Some(DomainKnowledgeContentType::Glossary)
    );
    assert_eq!(
        DomainKnowledgeContentType::parse_str("person_name"),
        Some(DomainKnowledgeContentType::PersonName)
    );
    assert_eq!(DomainKnowledgeContentType::parse_str("secret"), None);
}

#[test]
fn domain_knowledge_sql_scopes_to_active_tenant_and_authenticated_guild() {
    assert!(LIST_DOMAIN_KNOWLEDGE_SQL.contains("WHERE guild_id = $1"));
    assert!(LIST_DOMAIN_KNOWLEDGE_SQL.contains("tg.status = 'active'"));
    assert!(LIST_DOMAIN_KNOWLEDGE_SQL.contains("t.status = 'active'"));
    assert!(LIST_DOMAIN_KNOWLEDGE_SQL.contains("tenant_id = (SELECT tenant_id FROM active_tenant)"));
    assert!(!LIST_DOMAIN_KNOWLEDGE_SQL.contains("tenant_id IS NULL"));
    assert!(LIST_DOMAIN_KNOWLEDGE_SQL.contains("archived_at IS NULL"));
    assert!(INSERT_DOMAIN_KNOWLEDGE_SQL.contains("FROM active_tenant"));
    assert!(!INSERT_DOMAIN_KNOWLEDGE_SQL.contains("(SELECT tenant_id FROM active_tenant), $2"));
}

#[test]
fn domain_knowledge_mutations_increment_version_and_preserve_archive_contract() {
    assert!(UPDATE_DOMAIN_KNOWLEDGE_SQL.contains("COALESCE(NULLIF($6, '')::TEXT::BOOLEAN, active)"));
    assert!(UPDATE_DOMAIN_KNOWLEDGE_SQL.contains("THEN version + 1"));
    assert!(UPDATE_DOMAIN_KNOWLEDGE_SQL.contains("AND guild_id = $2"));
    assert!(UPDATE_DOMAIN_KNOWLEDGE_SQL.contains("AND archived_at IS NULL"));
    assert!(ACTIVATE_DOMAIN_KNOWLEDGE_SQL.contains("active = TRUE"));
    assert!(ACTIVATE_DOMAIN_KNOWLEDGE_SQL.contains("archived_at = NULL"));
    assert!(ACTIVATE_DOMAIN_KNOWLEDGE_SQL.contains("WHEN NOT active OR archived_at IS NOT NULL"));
    assert!(ARCHIVE_DOMAIN_KNOWLEDGE_SQL.contains("WHEN archived_at IS NULL THEN FALSE"));
    assert!(ARCHIVE_DOMAIN_KNOWLEDGE_SQL.contains("archived_at = COALESCE(archived_at, NOW())"));
    assert!(ARCHIVE_DOMAIN_KNOWLEDGE_SQL.contains("WHEN archived_at IS NULL THEN version + 1"));
    assert!(!ARCHIVE_DOMAIN_KNOWLEDGE_SQL.contains("AND archived_at IS NULL"));
}

#[test]
fn sql_store_lists_domain_knowledge_with_filters_and_parses_rows() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{LIST_DOMAIN_KNOWLEDGE_SQL}|g1\u{1f}false\u{1f}glossary"),
        vec![domain_row(
            "dk-1",
            Some("tenant-g1"),
            "g1",
            "glossary",
            true,
            3,
            None,
        )],
    );
    let mut store = SqlMeetingStore::new(executor);

    let items = store
        .list_domain_knowledge("g1", false, Some(DomainKnowledgeContentType::Glossary))
        .expect("domain knowledge rows should parse");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "dk-1");
    assert_eq!(items[0].tenant_id.as_deref(), Some("tenant-g1"));
    assert_eq!(items[0].guild_id, "g1");
    assert_eq!(items[0].content_type, DomainKnowledgeContentType::Glossary);
    assert_eq!(items[0].version, 3);
    assert!(items[0].archived_at.is_none());
    assert_eq!(
        store.executor.executed[0].1,
        vec!["g1".to_owned(), "false".to_owned(), "glossary".to_owned()]
    );
}

#[test]
fn sql_store_creates_updates_activates_and_archives_domain_knowledge() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!(
            "{INSERT_DOMAIN_KNOWLEDGE_SQL}|dk-1\u{1f}g1\u{1f}wording_rule\u{1f}Names\u{1f}Use project names.\u{1f}true\u{1f}actor-1"
        ),
        vec![domain_row("dk-1", Some("tenant-g1"), "g1", "wording_rule", true, 1, None)],
    );
    executor.query_rows_result.insert(
        format!(
            "{UPDATE_DOMAIN_KNOWLEDGE_SQL}|dk-1\u{1f}g1\u{1f}project_context\u{1f}Context\u{1f}A project context.\u{1f}false\u{1f}actor-2"
        ),
        vec![domain_row("dk-1", Some("tenant-g1"), "g1", "project_context", false, 2, None)],
    );
    executor.query_rows_result.insert(
        format!("{ACTIVATE_DOMAIN_KNOWLEDGE_SQL}|dk-1\u{1f}g1\u{1f}actor-3"),
        vec![domain_row("dk-1", Some("tenant-g1"), "g1", "project_context", true, 3, None)],
    );
    executor.query_rows_result.insert(
        format!("{ARCHIVE_DOMAIN_KNOWLEDGE_SQL}|dk-1\u{1f}g1\u{1f}actor-4"),
        vec![domain_row(
            "dk-1",
            Some("tenant-g1"),
            "g1",
            "project_context",
            false,
            4,
            Some("2026-06-03T02:00:00.000Z"),
        )],
    );
    let mut store = SqlMeetingStore::new(executor);

    let created = store
        .create_domain_knowledge(&NewDomainKnowledgeItem {
            id: "dk-1".to_owned(),
            guild_id: "g1".to_owned(),
            content_type: DomainKnowledgeContentType::WordingRule,
            title: "Names".to_owned(),
            body: "Use project names.".to_owned(),
            active: true,
            updated_actor_user_id: Some("actor-1".to_owned()),
        })
        .expect("create should parse returned row");
    assert_eq!(created.version, 1);

    let updated = store
        .update_domain_knowledge(&UpdateDomainKnowledgeItem {
            id: "dk-1".to_owned(),
            guild_id: "g1".to_owned(),
            content_type: DomainKnowledgeContentType::ProjectContext,
            title: "Context".to_owned(),
            body: "A project context.".to_owned(),
            active: false,
            updated_actor_user_id: Some("actor-2".to_owned()),
        })
        .expect("update should parse")
        .expect("row should exist");
    assert_eq!(updated.version, 2);
    assert!(!updated.active);

    let activated = store
        .activate_domain_knowledge("g1", "dk-1", Some("actor-3"))
        .expect("activate should parse")
        .expect("row should exist");
    assert!(activated.active);
    assert_eq!(activated.version, 3);

    let archived = store
        .archive_domain_knowledge("g1", "dk-1", "actor-4")
        .expect("archive should parse")
        .expect("row should exist");
    assert!(!archived.active);
    assert_eq!(archived.version, 4);
    assert!(archived.archived_at.is_some());
    assert_eq!(archived.archived_actor_user_id.as_deref(), Some("actor-2"));
}

#[test]
fn sql_store_requires_archive_actor_before_querying() {
    let executor = FakeSqlExecutor::default();
    let mut store = SqlMeetingStore::new(executor);

    let err = store
        .archive_domain_knowledge("g1", "dk-1", "")
        .expect_err("blank archive actor should be rejected");

    assert!(err.to_string().contains("archive actor is required"));
    assert!(store.executor.executed.is_empty());
}

#[test]
fn sql_store_rejects_invalid_domain_knowledge_rows() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{LIST_DOMAIN_KNOWLEDGE_SQL}|g1\u{1f}true\u{1f}"),
        vec![domain_row("dk-1", None, "g1", "invalid", true, 1, None)],
    );
    let mut store = SqlMeetingStore::new(executor);

    let err = store
        .list_domain_knowledge("g1", true, None)
        .expect_err("invalid content type should be rejected");

    assert!(err.to_string().contains("invalid domain knowledge content_type"));
}
