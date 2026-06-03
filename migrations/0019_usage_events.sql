CREATE TABLE IF NOT EXISTS usage_events (
    id TEXT PRIMARY KEY,
    tenant_id TEXT REFERENCES tenants(id) ON DELETE SET NULL,
    guild_id TEXT NOT NULL,
    meeting_id TEXT,
    job_id TEXT,
    resource_type TEXT,
    resource_id TEXT,
    metric TEXT NOT NULL,
    quantity BIGINT NOT NULL,
    detail_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    ALTER TABLE usage_events
    ADD CONSTRAINT usage_events_guild_id_nonempty_check
    CHECK (length(btrim(guild_id)) > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE usage_events
    ADD CONSTRAINT usage_events_metric_check
    CHECK (
        metric IN (
            'recording_minutes',
            'asr_seconds',
            'summary_runs',
            'storage_bytes',
            'debug_downloads'
        )
    );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE usage_events
    ADD CONSTRAINT usage_events_quantity_nonnegative_check
    CHECK (quantity >= 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE usage_events
    ADD CONSTRAINT usage_events_detail_json_object_check
    CHECK (jsonb_typeof(detail_json) = 'object');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS idx_usage_events_tenant_recent
    ON usage_events (tenant_id, observed_at DESC, id DESC)
    WHERE tenant_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_usage_events_guild_recent
    ON usage_events (guild_id, observed_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_usage_events_metric_recent
    ON usage_events (metric, observed_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_usage_events_meeting_metric
    ON usage_events (meeting_id, metric, observed_at DESC)
    WHERE meeting_id IS NOT NULL;
