use chrono::{TimeZone, Utc};
use discord_transcript::domain::plans::{
    PlanFallback, PlanKind, QuotaDimension, QuotaEnforcementMode, QuotaLimit, QuotaPeriod,
};
use discord_transcript::infrastructure::sql::{
    INCREMENTAL_MIGRATIONS_SQL, RESOLVE_PLAN_FOR_GUILD_SQL,
};
use discord_transcript::infrastructure::sql_store::{FakeSqlExecutor, SqlMeetingStore, SqlRow};

fn sql_row(values: Vec<Option<&str>>) -> SqlRow {
    values
        .into_iter()
        .map(|value| value.map(str::to_owned))
        .collect()
}

fn assigned_quota_row(quota_id: &str, dimension: &str, limit: Option<&str>, mode: &str) -> SqlRow {
    sql_row(vec![
        Some("assign-1"),
        Some("tenant-g1"),
        Some("g1"),
        Some("plan-pro"),
        Some("pro"),
        Some("Pro"),
        Some("custom"),
        Some("assignment"),
        Some("admin"),
        Some("2026-06-01T00:00:00.000Z"),
        Some("2026-07-01T00:00:00.000Z"),
        Some(quota_id),
        Some(dimension),
        Some("monthly"),
        limit,
        Some(if limit.is_some() { "false" } else { "true" }),
        Some(mode),
    ])
}

#[test]
fn incremental_migrations_include_plan_quota_assignment_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS plans"));
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS plan_quotas"));
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS guild_plan_assignments"));
    assert!(schema.contains("plan_quotas_limit_check"));
    assert!(schema.contains("observe_only"));
    assert!(schema.contains("'enforce'"));
    assert!(schema.contains("'daily'"));
    assert!(schema.contains("'monthly'"));
    assert!(schema.contains("'total'"));
    assert!(schema.contains("'current'"));
    assert!(schema.contains("guild_plan_assignments_no_active_overlap"));
    assert!(schema.contains("EXCLUDE USING gist"));
    assert!(schema.contains("'plan:default'"));
    assert!(schema.contains("'plan:beta'"));
}

#[test]
fn resolve_plan_sql_scopes_to_active_tenant_and_valid_assignment() {
    let sql = RESOLVE_PLAN_FOR_GUILD_SQL;

    assert!(sql.contains("tg.guild_id = $1"));
    assert!(sql.contains("tg.status = 'active'"));
    assert!(sql.contains("t.status = 'active'"));
    assert!(sql.contains("gpa.tenant_id = (SELECT tenant_id FROM active_tenant)"));
    assert!(sql.contains("p.status IN ('active', 'archived')"));
    assert!(sql.contains("gpa.valid_from <= $2::TEXT::TIMESTAMPTZ"));
    assert!(sql.contains("gpa.valid_until IS NULL OR gpa.valid_until > $2::TEXT::TIMESTAMPTZ"));
    assert!(sql.contains("p.code = $3"));
    assert!(sql.contains("p.code = 'default'"));
    assert!(sql.contains("EXISTS (SELECT 1 FROM active_tenant)"));
}

#[test]
fn sql_store_resolves_active_assignment_with_quotas() {
    let at = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!(
            "{RESOLVE_PLAN_FOR_GUILD_SQL}|g1\u{1f}{}\u{1f}default",
            at.to_rfc3339()
        ),
        vec![
            assigned_quota_row("quota-1", "recording_minutes", Some("100"), "observe_only"),
            assigned_quota_row("quota-2", "summary_runs", None, "enforce"),
        ],
    );

    let mut store = SqlMeetingStore::new(executor);
    let resolved = store
        .resolve_plan_for_guild("g1", PlanFallback::Default, at)
        .expect("plan resolver should parse")
        .expect("plan should resolve");

    assert_eq!(resolved.assignment_id.as_deref(), Some("assign-1"));
    assert_eq!(resolved.tenant_id.as_deref(), Some("tenant-g1"));
    assert_eq!(resolved.plan_code, "pro");
    assert_eq!(resolved.plan_kind, PlanKind::Custom);
    assert_eq!(resolved.resolution_source, "assignment");
    assert_eq!(
        resolved.valid_from.expect("valid_from").to_rfc3339(),
        "2026-06-01T00:00:00+00:00"
    );
    assert_eq!(resolved.quotas.len(), 2);
    assert_eq!(resolved.quotas[0].dimension, QuotaDimension::RecordingMinutes);
    assert_eq!(resolved.quotas[0].limit, QuotaLimit::Finite(100));
    assert_eq!(
        resolved.quotas[0].enforcement_mode,
        QuotaEnforcementMode::ObserveOnly
    );
    assert_eq!(resolved.quotas[1].dimension, QuotaDimension::SummaryRuns);
    assert_eq!(resolved.quotas[1].limit, QuotaLimit::Unlimited);
    assert_eq!(resolved.quotas[1].enforcement_mode, QuotaEnforcementMode::Enforce);
}

#[test]
fn sql_store_resolves_beta_and_default_fallbacks() {
    let at = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!(
            "{RESOLVE_PLAN_FOR_GUILD_SQL}|g1\u{1f}{}\u{1f}beta",
            at.to_rfc3339()
        ),
        vec![sql_row(vec![
            None,
            Some("tenant-g1"),
            Some("g1"),
            Some("plan:beta"),
            Some("beta"),
            Some("Beta"),
            Some("beta"),
            Some("fallback"),
            None,
            None,
            None,
            Some("quota:beta:summary_runs:monthly"),
            Some("summary_runs"),
            Some("monthly"),
            None,
            Some("true"),
            Some("observe_only"),
        ])],
    );
    executor.query_rows_result.insert(
        format!(
            "{RESOLVE_PLAN_FOR_GUILD_SQL}|g1\u{1f}{}\u{1f}default",
            at.to_rfc3339()
        ),
        vec![sql_row(vec![
            None,
            Some("tenant-g1"),
            Some("g1"),
            Some("plan:default"),
            Some("default"),
            Some("Default"),
            Some("default"),
            Some("fallback"),
            None,
            None,
            None,
            Some("quota:default:summary_runs:monthly"),
            Some("summary_runs"),
            Some("monthly"),
            None,
            Some("true"),
            Some("observe_only"),
        ])],
    );

    let mut store = SqlMeetingStore::new(executor);
    let beta = store
        .resolve_plan_for_guild("g1", PlanFallback::Beta, at)
        .expect("beta fallback should parse")
        .expect("beta plan should resolve");
    let default = store
        .resolve_plan_for_guild("g1", PlanFallback::Default, at)
        .expect("default fallback should parse")
        .expect("default plan should resolve");

    assert_eq!(beta.plan_code, "beta");
    assert_eq!(beta.plan_kind, PlanKind::Beta);
    assert_eq!(beta.resolution_source, "fallback");
    assert_eq!(default.plan_code, "default");
    assert_eq!(default.plan_kind, PlanKind::Default);
}

#[test]
fn sql_store_returns_none_when_no_plan_can_resolve() {
    let at = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
    let executor = FakeSqlExecutor::default();
    let mut store = SqlMeetingStore::new(executor);

    let resolved = store
        .resolve_plan_for_guild("missing", PlanFallback::Default, at)
        .expect("empty fake result should parse");

    assert_eq!(resolved, None);
}

#[test]
fn sql_store_rejects_inconsistent_plan_resolver_rows() {
    let at = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!(
            "{RESOLVE_PLAN_FOR_GUILD_SQL}|g1\u{1f}{}\u{1f}default",
            at.to_rfc3339()
        ),
        vec![
            assigned_quota_row("quota-1", "recording_minutes", Some("100"), "observe_only"),
            {
                let mut row = assigned_quota_row("quota-2", "summary_runs", Some("10"), "enforce");
                row[0] = Some("assign-2".to_owned());
                row
            },
        ],
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .resolve_plan_for_guild("g1", PlanFallback::Default, at)
        .expect_err("mixed assignments should be rejected");

    assert!(err.to_string().contains("multiple plans or assignments"));
}

#[test]
fn sql_store_rejects_invalid_quota_limit_rows() {
    let at = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!(
            "{RESOLVE_PLAN_FOR_GUILD_SQL}|g1\u{1f}{}\u{1f}default",
            at.to_rfc3339()
        ),
        vec![assigned_quota_row(
            "quota-1",
            "recording_minutes",
            Some("-1"),
            "observe_only",
        )],
    );

    let mut store = SqlMeetingStore::new(executor);
    let err = store
        .resolve_plan_for_guild("g1", PlanFallback::Default, at)
        .expect_err("negative finite limits should be rejected");

    assert!(err.to_string().contains("finite quota limit must be nonnegative"));
}

#[test]
fn plan_period_and_enforcement_values_round_trip() {
    assert_eq!(QuotaPeriod::Daily.as_str(), "daily");
    assert_eq!(QuotaPeriod::Monthly.as_str(), "monthly");
    assert_eq!(QuotaPeriod::Total.as_str(), "total");
    assert_eq!(QuotaPeriod::Current.as_str(), "current");
    assert_eq!(QuotaEnforcementMode::ObserveOnly.as_str(), "observe_only");
    assert_eq!(QuotaEnforcementMode::Enforce.as_str(), "enforce");
}
