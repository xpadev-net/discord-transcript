CREATE TABLE IF NOT EXISTS tenants (
    id TEXT PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'active',
    period_anchor TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    ALTER TABLE tenants
    ADD CONSTRAINT tenants_status_check
    CHECK (status IN ('active', 'suspended', 'closed'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE TABLE IF NOT EXISTS tenant_discord_guilds (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    guild_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    effective_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at TIMESTAMPTZ,
    assigned_by_user_id TEXT,
    source TEXT NOT NULL DEFAULT 'system',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    ALTER TABLE tenant_discord_guilds
    ADD CONSTRAINT tenant_discord_guilds_status_check
    CHECK (status IN ('active', 'revoked'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE tenant_discord_guilds
    ADD CONSTRAINT tenant_discord_guilds_source_check
    CHECK (source IN ('system', 'admin', 'billing_provider', 'migration'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE tenant_discord_guilds
    ADD CONSTRAINT tenant_discord_guilds_admin_actor_check
    CHECK (source <> 'admin' OR assigned_by_user_id IS NOT NULL);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE tenant_discord_guilds
    ADD CONSTRAINT tenant_discord_guilds_revocation_check
    CHECK (
        (status = 'active' AND revoked_at IS NULL)
        OR (status = 'revoked' AND revoked_at IS NOT NULL)
    );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE UNIQUE INDEX IF NOT EXISTS idx_tenant_discord_guilds_active_guild
    ON tenant_discord_guilds (guild_id)
    WHERE status = 'active';

CREATE UNIQUE INDEX IF NOT EXISTS idx_tenant_discord_guilds_active_pair
    ON tenant_discord_guilds (tenant_id, guild_id)
    WHERE status = 'active';

CREATE INDEX IF NOT EXISTS idx_tenant_discord_guilds_tenant_status
    ON tenant_discord_guilds (tenant_id, status);

CREATE TABLE IF NOT EXISTS tenant_memberships (
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_tenant_memberships_user_id
    ON tenant_memberships (user_id);

DO $$
BEGIN
    ALTER TABLE tenant_memberships
    ADD CONSTRAINT tenant_memberships_role_check
    CHECK (role IN ('owner', 'admin', 'member'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE tenant_memberships
    ADD CONSTRAINT tenant_memberships_status_check
    CHECK (status IN ('active', 'revoked'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

WITH existing_guilds AS (
    SELECT DISTINCT guild_id
    FROM meetings
    WHERE guild_id IS NOT NULL
    UNION
    SELECT DISTINCT guild_id
    FROM guild_settings
    WHERE guild_id IS NOT NULL
)
INSERT INTO tenants (id, status, created_at, updated_at)
SELECT guild_id, 'active', NOW(), NOW()
FROM existing_guilds
ON CONFLICT (id) DO NOTHING;

WITH existing_guilds AS (
    SELECT DISTINCT guild_id
    FROM meetings
    WHERE guild_id IS NOT NULL
    UNION
    SELECT DISTINCT guild_id
    FROM guild_settings
    WHERE guild_id IS NOT NULL
)
INSERT INTO tenant_discord_guilds (
    id, tenant_id, guild_id, status, effective_at, source, created_at, updated_at
)
SELECT
    'migration:default-tenant:' || guild_id,
    guild_id,
    guild_id,
    'active',
    NOW(),
    'migration',
    NOW(),
    NOW()
FROM existing_guilds
WHERE NOT EXISTS (
    SELECT 1
    FROM tenant_discord_guilds existing
    WHERE existing.guild_id = existing_guilds.guild_id
)
ON CONFLICT (id) DO NOTHING;
