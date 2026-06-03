CREATE TABLE IF NOT EXISTS summary_templates (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    guild_id TEXT NOT NULL,
    name TEXT NOT NULL,
    template TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    version INTEGER NOT NULL DEFAULT 1,
    updated_actor_user_id TEXT,
    archived_at TIMESTAMPTZ,
    archived_actor_user_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT summary_templates_name_not_blank_check CHECK (length(btrim(name)) > 0),
    CONSTRAINT summary_templates_template_not_blank_check CHECK (length(btrim(template)) > 0),
    CONSTRAINT summary_templates_version_positive_check CHECK (version >= 1),
    CONSTRAINT summary_templates_archived_actor_check CHECK (
        archived_at IS NULL OR archived_actor_user_id IS NOT NULL
    )
);

CREATE INDEX IF NOT EXISTS idx_summary_templates_tenant_guild_active
    ON summary_templates (tenant_id, guild_id, active, updated_at DESC, id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_summary_templates_one_active_per_guild
    ON summary_templates (tenant_id, guild_id)
    WHERE active = TRUE AND archived_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_summary_templates_guild_updated
    ON summary_templates (guild_id, updated_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_summary_templates_guild_unarchived
    ON summary_templates (guild_id, archived_at, updated_at DESC, id);
