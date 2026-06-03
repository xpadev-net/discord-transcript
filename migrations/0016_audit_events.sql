CREATE TABLE IF NOT EXISTS audit_events (
    id TEXT PRIMARY KEY,
    tenant_id TEXT REFERENCES tenants(id) ON DELETE SET NULL,
    guild_id TEXT,
    actor_user_id TEXT,
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT,
    request_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    detail_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    ALTER TABLE audit_events
    ADD CONSTRAINT audit_events_action_nonempty_check
    CHECK (length(btrim(action)) > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE audit_events
    ADD CONSTRAINT audit_events_resource_type_nonempty_check
    CHECK (length(btrim(resource_type)) > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE audit_events
    ADD CONSTRAINT audit_events_request_metadata_object_check
    CHECK (jsonb_typeof(request_metadata) = 'object');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE audit_events
    ADD CONSTRAINT audit_events_detail_json_object_check
    CHECK (jsonb_typeof(detail_json) = 'object');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS idx_audit_events_tenant_recent
    ON audit_events (tenant_id, occurred_at DESC, id DESC)
    WHERE tenant_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_audit_events_guild_recent
    ON audit_events (guild_id, occurred_at DESC, id DESC)
    WHERE guild_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_audit_events_actor_recent
    ON audit_events (actor_user_id, occurred_at DESC, id DESC)
    WHERE actor_user_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_audit_events_action_recent
    ON audit_events (action, occurred_at DESC, id DESC);
