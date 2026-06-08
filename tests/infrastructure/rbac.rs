use discord_transcript::domain::authz::RbacPermission;
use discord_transcript::infrastructure::sql::{
    INCREMENTAL_MIGRATIONS_SQL, LIST_GUILD_RBAC_PERMISSIONS_FOR_ROLES_SQL, MIGRATIONS,
};

fn guild_rbac_migration_sql() -> &'static str {
    MIGRATIONS
        .iter()
        .find(|migration| migration.version == "0025_guild_rbac")
        .expect("guild RBAC migration should be registered")
        .sql
}

#[test]
fn migrations_register_guild_rbac_schema_after_job_operations() {
    let versions = MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();

    let job_operations = versions
        .iter()
        .position(|version| *version == "0024_job_operations")
        .expect("job operations migration should be registered");
    let guild_rbac = versions
        .iter()
        .position(|version| *version == "0025_guild_rbac")
        .expect("guild RBAC migration should be registered");

    assert!(guild_rbac > job_operations);
}

#[test]
fn incremental_migrations_include_guild_rbac_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS guild_rbac_role_bindings"));
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS guild_rbac_permissions"));
    assert!(schema.contains("guild_rbac_permissions_role_binding_fk"));
    assert!(schema.contains("CREATE OR REPLACE FUNCTION touch_guild_rbac_updated_at()"));
    assert!(schema.contains("trg_guild_rbac_role_bindings_updated_at"));
    assert!(schema.contains("trg_guild_rbac_permissions_updated_at"));
    assert!(schema.contains("idx_guild_rbac_permissions_guild_permission"));
    assert!(!schema.contains("idx_guild_rbac_permissions_guild_role"));
}

#[test]
fn guild_rbac_migration_defines_all_domain_permissions() {
    let schema = guild_rbac_migration_sql();

    for permission in RbacPermission::ALL {
        assert!(
            schema.contains(permission.as_str()),
            "migration must allow {}",
            permission.as_str()
        );
    }
}

#[test]
fn guild_rbac_role_lookup_filters_by_guild_roles_and_active_bindings() {
    let sql = LIST_GUILD_RBAC_PERMISSIONS_FOR_ROLES_SQL;

    assert!(sql.contains("permissions.guild_id = $1"));
    assert!(sql.contains("permissions.discord_role_id = ANY($2::TEXT[])"));
    assert!(sql.contains("bindings.active = TRUE"));
    assert!(sql.contains("ORDER BY permissions.discord_role_id, permissions.permission_name"));
}
