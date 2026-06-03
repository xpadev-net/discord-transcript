CREATE TABLE IF NOT EXISTS domain_knowledge_items (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    guild_id TEXT NOT NULL,
    content_type TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    version INTEGER NOT NULL DEFAULT 1,
    updated_actor_user_id TEXT,
    archived_at TIMESTAMPTZ,
    archived_actor_user_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT domain_knowledge_items_content_type_check CHECK (
        content_type IN (
            'glossary',
            'person_name',
            'project_context',
            'wording_rule',
            'prohibited_item'
        )
    ),
    CONSTRAINT domain_knowledge_items_version_positive_check CHECK (version >= 1),
    CONSTRAINT domain_knowledge_items_archived_actor_check CHECK (
        archived_at IS NULL OR archived_actor_user_id IS NOT NULL
    )
);

CREATE INDEX IF NOT EXISTS idx_domain_knowledge_tenant_guild_active
    ON domain_knowledge_items (tenant_id, guild_id, active, updated_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_domain_knowledge_guild_updated
    ON domain_knowledge_items (guild_id, updated_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_domain_knowledge_guild_type
    ON domain_knowledge_items (guild_id, content_type, active, updated_at DESC, id);
