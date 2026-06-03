CREATE EXTENSION IF NOT EXISTS btree_gist;

CREATE TABLE IF NOT EXISTS plans (
    id TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'custom',
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    ALTER TABLE plans
    ADD CONSTRAINT plans_code_nonempty_check
    CHECK (length(btrim(code)) > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE plans
    ADD CONSTRAINT plans_name_nonempty_check
    CHECK (length(btrim(name)) > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE plans
    ADD CONSTRAINT plans_kind_check
    CHECK (kind IN ('default', 'beta', 'custom'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE plans
    ADD CONSTRAINT plans_status_check
    CHECK (status IN ('active', 'archived'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE TABLE IF NOT EXISTS plan_quotas (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    dimension TEXT NOT NULL,
    period TEXT NOT NULL,
    limit_value BIGINT,
    unlimited BOOLEAN NOT NULL DEFAULT false,
    enforcement_mode TEXT NOT NULL DEFAULT 'observe_only',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    ALTER TABLE plan_quotas
    ADD CONSTRAINT plan_quotas_dimension_check
    CHECK (
        dimension IN (
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
    ALTER TABLE plan_quotas
    ADD CONSTRAINT plan_quotas_period_check
    CHECK (period IN ('daily', 'monthly', 'total', 'current'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE plan_quotas
    ADD CONSTRAINT plan_quotas_limit_check
    CHECK (
        (unlimited = true AND limit_value IS NULL)
        OR (unlimited = false AND limit_value IS NOT NULL AND limit_value >= 0)
    );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE plan_quotas
    ADD CONSTRAINT plan_quotas_enforcement_mode_check
    CHECK (enforcement_mode IN ('observe_only', 'enforce'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_quotas_plan_dimension_period
    ON plan_quotas (plan_id, dimension, period);

CREATE TABLE IF NOT EXISTS guild_plan_assignments (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    guild_id TEXT NOT NULL,
    plan_id TEXT NOT NULL REFERENCES plans(id) ON DELETE RESTRICT,
    status TEXT NOT NULL DEFAULT 'active',
    valid_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    valid_until TIMESTAMPTZ,
    assigned_by_user_id TEXT,
    source TEXT NOT NULL DEFAULT 'system',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    ALTER TABLE guild_plan_assignments
    ADD CONSTRAINT guild_plan_assignments_guild_id_nonempty_check
    CHECK (length(btrim(guild_id)) > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE guild_plan_assignments
    ADD CONSTRAINT guild_plan_assignments_status_check
    CHECK (status IN ('active', 'revoked'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE guild_plan_assignments
    ADD CONSTRAINT guild_plan_assignments_source_check
    CHECK (source IN ('system', 'admin', 'billing_provider', 'migration'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE guild_plan_assignments
    ADD CONSTRAINT guild_plan_assignments_admin_actor_check
    CHECK (source <> 'admin' OR assigned_by_user_id IS NOT NULL);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE guild_plan_assignments
    ADD CONSTRAINT guild_plan_assignments_valid_time_check
    CHECK (valid_until IS NULL OR valid_until > valid_from);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE guild_plan_assignments
    ADD CONSTRAINT guild_plan_assignments_no_active_overlap
    EXCLUDE USING gist (
        guild_id WITH =,
        tstzrange(valid_from, COALESCE(valid_until, 'infinity'::timestamptz), '[)') WITH &&
    )
    WHERE (status = 'active');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS idx_guild_plan_assignments_tenant_status
    ON guild_plan_assignments (tenant_id, status, valid_from DESC);

CREATE INDEX IF NOT EXISTS idx_guild_plan_assignments_guild_validity
    ON guild_plan_assignments (guild_id, status, valid_from DESC);

INSERT INTO plans (id, code, name, kind, status, created_at, updated_at)
VALUES
    ('plan:default', 'default', 'Default', 'default', 'active', NOW(), NOW()),
    ('plan:beta', 'beta', 'Beta', 'beta', 'active', NOW(), NOW())
ON CONFLICT DO NOTHING;

INSERT INTO plan_quotas (
    id, plan_id, dimension, period, limit_value, unlimited, enforcement_mode, created_at, updated_at
)
VALUES
    ('quota:default:recording_minutes:monthly', 'plan:default', 'recording_minutes', 'monthly', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:default:asr_seconds:monthly', 'plan:default', 'asr_seconds', 'monthly', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:default:summary_runs:monthly', 'plan:default', 'summary_runs', 'monthly', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:default:storage_bytes:current', 'plan:default', 'storage_bytes', 'current', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:default:debug_downloads:daily', 'plan:default', 'debug_downloads', 'daily', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:beta:recording_minutes:monthly', 'plan:beta', 'recording_minutes', 'monthly', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:beta:asr_seconds:monthly', 'plan:beta', 'asr_seconds', 'monthly', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:beta:summary_runs:monthly', 'plan:beta', 'summary_runs', 'monthly', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:beta:storage_bytes:current', 'plan:beta', 'storage_bytes', 'current', NULL, true, 'observe_only', NOW(), NOW()),
    ('quota:beta:debug_downloads:daily', 'plan:beta', 'debug_downloads', 'daily', NULL, true, 'observe_only', NOW(), NOW())
ON CONFLICT DO NOTHING;
