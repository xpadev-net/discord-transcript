CREATE TABLE IF NOT EXISTS guild_rbac_role_bindings (
    guild_id TEXT NOT NULL,
    discord_role_id TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_actor_user_id TEXT,
    updated_actor_user_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (guild_id, discord_role_id),
    CONSTRAINT guild_rbac_role_bindings_guild_id_nonempty_check
        CHECK (length(btrim(guild_id)) > 0),
    CONSTRAINT guild_rbac_role_bindings_discord_role_id_nonempty_check
        CHECK (length(btrim(discord_role_id)) > 0),
    CONSTRAINT guild_rbac_role_bindings_created_actor_nonempty_check
        CHECK (created_actor_user_id IS NULL OR length(btrim(created_actor_user_id)) > 0),
    CONSTRAINT guild_rbac_role_bindings_updated_actor_nonempty_check
        CHECK (updated_actor_user_id IS NULL OR length(btrim(updated_actor_user_id)) > 0)
);

CREATE TABLE IF NOT EXISTS guild_rbac_permissions (
    guild_id TEXT NOT NULL,
    discord_role_id TEXT NOT NULL,
    permission_name TEXT NOT NULL,
    created_actor_user_id TEXT,
    updated_actor_user_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (guild_id, discord_role_id, permission_name),
    CONSTRAINT guild_rbac_permissions_role_binding_fk
        FOREIGN KEY (guild_id, discord_role_id)
        REFERENCES guild_rbac_role_bindings(guild_id, discord_role_id) ON DELETE CASCADE,
    CONSTRAINT guild_rbac_permissions_guild_id_nonempty_check
        CHECK (length(btrim(guild_id)) > 0),
    CONSTRAINT guild_rbac_permissions_discord_role_id_nonempty_check
        CHECK (length(btrim(discord_role_id)) > 0),
    CONSTRAINT guild_rbac_permissions_created_actor_nonempty_check
        CHECK (created_actor_user_id IS NULL OR length(btrim(created_actor_user_id)) > 0),
    CONSTRAINT guild_rbac_permissions_updated_actor_nonempty_check
        CHECK (updated_actor_user_id IS NULL OR length(btrim(updated_actor_user_id)) > 0),
    CONSTRAINT guild_rbac_permissions_name_check
        CHECK (
            permission_name IN (
                'recording:start',
                'recording:stop',
                'meeting:view',
                'meeting:reprocess',
                'meeting:delete',
                'settings:manage',
                'summary_template:manage',
                'domain_knowledge:manage',
                'usage:view',
                'admin:view'
            )
        )
);

CREATE INDEX IF NOT EXISTS idx_guild_rbac_role_bindings_guild_active
    ON guild_rbac_role_bindings (guild_id, active, updated_at DESC, discord_role_id);

CREATE INDEX IF NOT EXISTS idx_guild_rbac_permissions_guild_role
    ON guild_rbac_permissions (guild_id, discord_role_id);

CREATE INDEX IF NOT EXISTS idx_guild_rbac_permissions_guild_permission
    ON guild_rbac_permissions (guild_id, permission_name, discord_role_id);
