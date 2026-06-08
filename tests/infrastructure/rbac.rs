use discord_transcript::domain::authz::RbacPermission;
use discord_transcript::infrastructure::sql::{
    INCREMENTAL_MIGRATIONS_SQL, LIST_GUILD_RBAC_PERMISSIONS_FOR_ROLES_SQL,
    LIST_GUILD_RBAC_ROLE_GRANTS_SQL, MIGRATIONS, RESET_GUILD_RBAC_ROLE_GRANT_SQL,
    UPSERT_GUILD_RBAC_ROLE_GRANT_SQL,
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

#[test]
fn guild_rbac_management_sql_lists_only_active_grants() {
    let sql = LIST_GUILD_RBAC_ROLE_GRANTS_SQL;

    assert!(sql.contains("bindings.guild_id = $1"));
    assert!(sql.contains("bindings.active = TRUE"));
    assert!(sql.contains("array_agg(permissions.permission_name"));
    assert!(sql.contains("created_actor_user_id"));
    assert!(sql.contains("updated_actor_user_id"));
}

#[test]
fn guild_rbac_management_sql_replaces_permission_set_atomically() {
    let sql = UPSERT_GUILD_RBAC_ROLE_GRANT_SQL;

    assert!(sql.contains("WITH normalized_permissions"));
    assert!(sql.contains("ON CONFLICT (guild_id, discord_role_id) DO UPDATE"));
    assert!(sql.contains("active = TRUE"));
    assert!(sql.contains("DELETE FROM guild_rbac_permissions"));
    assert!(sql.contains("permission_name NOT IN"));
    assert!(sql.contains("ON CONFLICT (guild_id, discord_role_id, permission_name) DO UPDATE"));
    assert!(sql.contains("SELECT array_agg(permission_name ORDER BY permission_name)"));
}

#[test]
fn guild_rbac_reset_sql_deactivates_binding_and_removes_permissions() {
    let sql = RESET_GUILD_RBAC_ROLE_GRANT_SQL;

    assert!(sql.contains("active = FALSE"));
    assert!(sql.contains("DELETE FROM guild_rbac_permissions"));
    assert!(sql.contains("removed_permission_names"));
    assert!(!sql.contains("DELETE FROM guild_rbac_role_bindings"));
}
