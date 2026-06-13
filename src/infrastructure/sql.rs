pub const INITIAL_SCHEMA_SQL: &str = include_str!("../../migrations/0001_mvp_schema.sql");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    pub version: &'static str,
    pub sql: &'static str,
}

pub const CREATE_SCHEMA_MIGRATIONS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version TEXT PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
)
"#;

pub const LOCK_SCHEMA_MIGRATIONS_SQL: &str = "SELECT pg_advisory_lock(760918997406360681)";
pub const UNLOCK_SCHEMA_MIGRATIONS_SQL: &str = "SELECT pg_advisory_unlock(760918997406360681)";

pub const SELECT_SCHEMA_MIGRATION_SQL: &str = "SELECT 1 FROM schema_migrations WHERE version = $1";

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: "0001_mvp_schema",
        sql: include_str!("../../migrations/0001_mvp_schema.sql"),
    },
    Migration {
        version: "0002_add_is_noisy",
        sql: include_str!("../../migrations/0002_add_is_noisy.sql"),
    },
    Migration {
        version: "0003_add_meeting_speakers",
        sql: include_str!("../../migrations/0003_add_meeting_speakers.sql"),
    },
    Migration {
        version: "0004_add_transcript_source",
        sql: include_str!("../../migrations/0004_add_transcript_source.sql"),
    },
    Migration {
        version: "0005_add_enum_constraints",
        sql: include_str!("../../migrations/0005_add_enum_constraints.sql"),
    },
    Migration {
        version: "0006_add_status_messages_and_retention",
        sql: include_str!("../../migrations/0006_add_status_messages_and_retention.sql"),
    },
    Migration {
        version: "0007_session_revocations",
        sql: include_str!("../../migrations/0007_session_revocations.sql"),
    },
    Migration {
        version: "0008_add_job_lease",
        sql: include_str!("../../migrations/0008_add_job_lease.sql"),
    },
    Migration {
        version: "0009_add_stop_reason_check",
        sql: include_str!("../../migrations/0009_add_stop_reason_check.sql"),
    },
    Migration {
        version: "0010_guild_settings",
        sql: include_str!("../../migrations/0010_guild_settings.sql"),
    },
    Migration {
        version: "0011_transcript_cursor_index",
        sql: include_str!("../../migrations/0011_transcript_cursor_index.sql"),
    },
    Migration {
        version: "0012_add_transcript_stage",
        sql: include_str!("../../migrations/0012_add_transcript_stage.sql"),
    },
    Migration {
        version: "0013_guild_bot_tokens",
        sql: include_str!("../../migrations/0013_guild_bot_tokens.sql"),
    },
    Migration {
        version: "0014_tenants_and_installations",
        sql: include_str!("../../migrations/0014_tenants_and_installations.sql"),
    },
    Migration {
        version: "0015_effective_meeting_settings",
        sql: include_str!("../../migrations/0015_effective_meeting_settings.sql"),
    },
    Migration {
        version: "0016_audit_events",
        sql: include_str!("../../migrations/0016_audit_events.sql"),
    },
    Migration {
        version: "0017_domain_knowledge",
        sql: include_str!("../../migrations/0017_domain_knowledge.sql"),
    },
    Migration {
        version: "0018_summary_templates",
        sql: include_str!("../../migrations/0018_summary_templates.sql"),
    },
    Migration {
        version: "0019_usage_events",
        sql: include_str!("../../migrations/0019_usage_events.sql"),
    },
    Migration {
        version: "0020_plans_and_quotas",
        sql: include_str!("../../migrations/0020_plans_and_quotas.sql"),
    },
    Migration {
        version: "0021_ai_memory_feedback",
        sql: include_str!("../../migrations/0021_ai_memory_feedback.sql"),
    },
    Migration {
        version: "0022_forward_fixups_for_0020_0021",
        sql: include_str!("../../migrations/0022_forward_fixups_for_0020_0021.sql"),
    },
    Migration {
        version: "0023_feedback_idempotency_quota",
        sql: include_str!("../../migrations/0023_feedback_idempotency_quota.sql"),
    },
    Migration {
        version: "0024_job_operations",
        sql: include_str!("../../migrations/0024_job_operations.sql"),
    },
    Migration {
        version: "0025_guild_rbac",
        sql: include_str!("../../migrations/0025_guild_rbac.sql"),
    },
    Migration {
        version: "0026_job_claim_token",
        sql: include_str!("../../migrations/0026_job_claim_token.sql"),
    },
    Migration {
        version: "0027_meeting_voice_channel_name",
        sql: include_str!("../../migrations/0027_meeting_voice_channel_name.sql"),
    },
];

pub fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub fn migration_transaction_sql(migration: Migration) -> String {
    format!(
        "BEGIN;\n{}\nINSERT INTO schema_migrations (version) VALUES ({}) ON CONFLICT (version) DO NOTHING;\nCOMMIT;",
        migration.sql.trim_end(),
        sql_literal(migration.version),
    )
}

/// Incremental migrations applied after the initial schema.
/// Each statement must be idempotent (IF NOT EXISTS / IF EXISTS).
pub const INCREMENTAL_MIGRATIONS_SQL: &str = concat!(
    include_str!("../../migrations/0002_add_is_noisy.sql"),
    "\n",
    include_str!("../../migrations/0003_add_meeting_speakers.sql"),
    "\n",
    include_str!("../../migrations/0004_add_transcript_source.sql"),
    "\n",
    include_str!("../../migrations/0005_add_enum_constraints.sql"),
    "\n",
    include_str!("../../migrations/0006_add_status_messages_and_retention.sql"),
    "\n",
    include_str!("../../migrations/0007_session_revocations.sql"),
    "\n",
    include_str!("../../migrations/0008_add_job_lease.sql"),
    "\n",
    include_str!("../../migrations/0009_add_stop_reason_check.sql"),
    "\n",
    include_str!("../../migrations/0010_guild_settings.sql"),
    "\n",
    include_str!("../../migrations/0011_transcript_cursor_index.sql"),
    "\n",
    include_str!("../../migrations/0012_add_transcript_stage.sql"),
    "\n",
    include_str!("../../migrations/0013_guild_bot_tokens.sql"),
    "\n",
    include_str!("../../migrations/0014_tenants_and_installations.sql"),
    "\n",
    include_str!("../../migrations/0015_effective_meeting_settings.sql"),
    "\n",
    include_str!("../../migrations/0016_audit_events.sql"),
    "\n",
    include_str!("../../migrations/0017_domain_knowledge.sql"),
    "\n",
    include_str!("../../migrations/0018_summary_templates.sql"),
    "\n",
    include_str!("../../migrations/0019_usage_events.sql"),
    "\n",
    include_str!("../../migrations/0020_plans_and_quotas.sql"),
    "\n",
    include_str!("../../migrations/0021_ai_memory_feedback.sql"),
    "\n",
    include_str!("../../migrations/0022_forward_fixups_for_0020_0021.sql"),
    "\n",
    include_str!("../../migrations/0023_feedback_idempotency_quota.sql"),
    "\n",
    include_str!("../../migrations/0024_job_operations.sql"),
    "\n",
    include_str!("../../migrations/0025_guild_rbac.sql"),
    "\n",
    include_str!("../../migrations/0026_job_claim_token.sql"),
    "\n",
    include_str!("../../migrations/0027_meeting_voice_channel_name.sql"),
);

pub const REVOKE_SESSION_SQL: &str = r#"
INSERT INTO session_revocations (user_id, issued_at)
VALUES ($1, $2)
ON CONFLICT (user_id, issued_at) DO NOTHING
"#;

pub const SESSION_IS_REVOKED_SQL: &str = r#"
SELECT 1
FROM session_revocations
WHERE user_id = $1
  AND issued_at = $2
LIMIT 1
"#;

pub const LIST_GUILD_RBAC_PERMISSIONS_FOR_ROLES_SQL: &str = r#"
SELECT
    permissions.guild_id,
    permissions.discord_role_id,
    permissions.permission_name
FROM guild_rbac_permissions permissions
JOIN guild_rbac_role_bindings bindings
  ON bindings.guild_id = permissions.guild_id
 AND bindings.discord_role_id = permissions.discord_role_id
WHERE permissions.guild_id = $1
  AND permissions.discord_role_id = ANY($2::TEXT[])
  AND bindings.active = TRUE
ORDER BY permissions.discord_role_id, permissions.permission_name
"#;

pub const LIST_GUILD_RBAC_PERMISSIONS_FOR_ROLE_CSV_SQL: &str = r#"
SELECT
    permissions.guild_id,
    permissions.discord_role_id,
    permissions.permission_name
FROM guild_rbac_permissions permissions
JOIN guild_rbac_role_bindings bindings
  ON bindings.guild_id = permissions.guild_id
 AND bindings.discord_role_id = permissions.discord_role_id
WHERE permissions.guild_id = $1
  AND permissions.discord_role_id = ANY(string_to_array($2, ','))
  AND bindings.active = TRUE
ORDER BY permissions.discord_role_id, permissions.permission_name
"#;

pub const LIST_GUILD_RBAC_ROLE_GRANTS_SQL: &str = r#"
SELECT
    bindings.guild_id,
    bindings.discord_role_id,
    COALESCE(
        array_remove(array_agg(permissions.permission_name ORDER BY permissions.permission_name), NULL),
        ARRAY[]::TEXT[]
    ) AS permission_names,
    bindings.created_actor_user_id,
    bindings.updated_actor_user_id,
    bindings.created_at::TEXT AS created_at,
    bindings.updated_at::TEXT AS updated_at
FROM guild_rbac_role_bindings bindings
LEFT JOIN guild_rbac_permissions permissions
  ON permissions.guild_id = bindings.guild_id
 AND permissions.discord_role_id = bindings.discord_role_id
WHERE bindings.guild_id = $1
  AND bindings.active = TRUE
GROUP BY
    bindings.guild_id,
    bindings.discord_role_id,
    bindings.created_actor_user_id,
    bindings.updated_actor_user_id,
    bindings.created_at,
    bindings.updated_at
ORDER BY bindings.discord_role_id
"#;

pub const UPSERT_GUILD_RBAC_ROLE_GRANT_SQL: &str = r#"
WITH normalized_permissions AS (
    SELECT DISTINCT permission_name
    FROM unnest($4::TEXT[]) AS permission_name
),
binding AS (
    INSERT INTO guild_rbac_role_bindings (
        guild_id,
        discord_role_id,
        active,
        created_actor_user_id,
        updated_actor_user_id
    )
    VALUES ($1, $2, TRUE, $3, $3)
    ON CONFLICT (guild_id, discord_role_id) DO UPDATE
    SET
        active = TRUE,
        updated_actor_user_id = $3
    RETURNING
        guild_id,
        discord_role_id,
        created_actor_user_id,
        updated_actor_user_id,
        created_at::TEXT AS created_at,
        updated_at::TEXT AS updated_at
),
removed_permissions AS (
    DELETE FROM guild_rbac_permissions permissions
    WHERE permissions.guild_id = $1
      AND permissions.discord_role_id = $2
      AND permissions.permission_name NOT IN (
          SELECT permission_name FROM normalized_permissions
      )
),
upserted_permissions AS (
    INSERT INTO guild_rbac_permissions (
        guild_id,
        discord_role_id,
        permission_name,
        created_actor_user_id,
        updated_actor_user_id
    )
    SELECT $1, $2, permission_name, $3, $3
    FROM normalized_permissions
    ON CONFLICT (guild_id, discord_role_id, permission_name) DO UPDATE
    SET updated_actor_user_id = $3
)
SELECT
    binding.guild_id,
    binding.discord_role_id,
    COALESCE(
        (
            SELECT array_agg(permission_name ORDER BY permission_name)
            FROM normalized_permissions
        ),
        ARRAY[]::TEXT[]
    ) AS permission_names,
    binding.created_actor_user_id,
    binding.updated_actor_user_id,
    binding.created_at,
    binding.updated_at
FROM binding
GROUP BY
    binding.guild_id,
    binding.discord_role_id,
    binding.created_actor_user_id,
    binding.updated_actor_user_id,
    binding.created_at,
    binding.updated_at
"#;

pub const RESET_GUILD_RBAC_ROLE_GRANT_SQL: &str = r#"
WITH binding AS (
    INSERT INTO guild_rbac_role_bindings (
        guild_id,
        discord_role_id,
        active,
        created_actor_user_id,
        updated_actor_user_id
    )
    VALUES ($1, $2, FALSE, $3, $3)
    ON CONFLICT (guild_id, discord_role_id) DO UPDATE
    SET
        active = FALSE,
        updated_actor_user_id = $3
    RETURNING
        guild_id,
        discord_role_id,
        created_actor_user_id,
        updated_actor_user_id,
        created_at::TEXT AS created_at,
        updated_at::TEXT AS updated_at
),
removed_permissions AS (
    DELETE FROM guild_rbac_permissions
    WHERE guild_id = $1
      AND discord_role_id = $2
    RETURNING permission_name
)
SELECT
    binding.guild_id,
    binding.discord_role_id,
    ARRAY[]::TEXT[] AS permission_names,
    binding.created_actor_user_id,
    binding.updated_actor_user_id,
    binding.created_at,
    binding.updated_at,
    COALESCE(
        array_remove(array_agg(removed_permissions.permission_name ORDER BY removed_permissions.permission_name), NULL),
        ARRAY[]::TEXT[]
    ) AS removed_permission_names
FROM binding
LEFT JOIN removed_permissions ON TRUE
GROUP BY
    binding.guild_id,
    binding.discord_role_id,
    binding.created_actor_user_id,
    binding.updated_actor_user_id,
    binding.created_at,
    binding.updated_at
"#;

pub const MARK_STOPPING_IF_RECORDING_SQL: &str = r#"
UPDATE meetings
SET
  status = 'stopping',
  stop_reason = $1,
  stopped_at = NOW(),
  meeting_duration_seconds = GREATEST(0, EXTRACT(EPOCH FROM (NOW() - started_at))::INTEGER),
  updated_at = NOW()
WHERE id = $2
  AND status = 'recording'
"#;

pub const INSERT_USAGE_EVENT_SQL: &str = r#"
INSERT INTO usage_events (
    id, tenant_id, guild_id, meeting_id, job_id, resource_type, resource_id,
    metric, quantity, detail_json, observed_at, created_at
)
SELECT
    $1,
    COALESCE(
        NULLIF($2, ''),
        (
            SELECT tenant_id
            FROM tenant_discord_guilds
            WHERE guild_id = $3
              AND status = 'active'
            ORDER BY effective_at DESC
            LIMIT 1
        )
    ),
    $3,
    NULLIF($4,''),
    NULLIF($5,''),
    NULLIF($6,''),
    NULLIF($7,''),
    $8,
    $9::TEXT::BIGINT,
    $10::TEXT::JSONB,
    $11::TEXT::TIMESTAMPTZ,
    NOW()
ON CONFLICT (id) DO NOTHING
"#;

pub const ADMIN_RETENTION_OVERVIEW_SQL: &str = r#"
WITH artifact_totals AS (
    SELECT
        COALESCE(SUM(a.size_bytes), 0)::BIGINT AS storage_bytes,
        COUNT(*)::BIGINT AS artifact_count,
        COALESCE(SUM(a.size_bytes) FILTER (WHERE a.kind IN ('raw_audio', 'audio', 'mixdown_audio', 'speaker_audio')), 0)::BIGINT AS raw_audio_bytes,
        COALESCE(SUM(a.size_bytes) FILTER (WHERE a.kind IN ('transcript', 'masked_transcript')), 0)::BIGINT AS transcript_bytes,
        COALESCE(SUM(a.size_bytes) FILTER (WHERE a.kind IN ('summary', 'summary_markdown')), 0)::BIGINT AS summary_bytes,
        COALESCE(SUM(a.size_bytes) FILTER (WHERE a.kind IN ('debug', 'debug_artifact', 'whisper_debug')), 0)::BIGINT AS debug_bytes
    FROM artifacts a
    JOIN meetings m ON m.id = a.meeting_id
    WHERE m.guild_id = $1
),
meeting_totals AS (
    SELECT
        COUNT(*)::BIGINT AS meeting_count,
        COUNT(*) FILTER (WHERE status IN ('recording', 'stopping', 'transcribing', 'summarizing'))::BIGINT AS active_meeting_count
    FROM meetings
    WHERE guild_id = $1
),
usage_totals AS (
    SELECT COALESCE(SUM(quantity), 0)::BIGINT AS observed_storage_bytes
    FROM usage_events
    WHERE guild_id = $1
      AND metric = 'storage_bytes'
)
SELECT
    meeting_totals.meeting_count,
    meeting_totals.active_meeting_count,
    artifact_totals.artifact_count,
    artifact_totals.storage_bytes,
    artifact_totals.raw_audio_bytes,
    artifact_totals.transcript_bytes,
    artifact_totals.summary_bytes,
    artifact_totals.debug_bytes,
    usage_totals.observed_storage_bytes
FROM artifact_totals, meeting_totals, usage_totals
"#;

pub const ADMIN_RETENTION_MEETING_DETAIL_SQL: &str = r#"
SELECT
    m.id,
    m.guild_id,
    m.voice_channel_id,
    m.status,
    to_char(m.started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS started_at,
    to_char(m.stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS stopped_at,
    COALESCE((SELECT COUNT(*) FROM transcripts t WHERE t.meeting_id = m.id AND NOT t.is_deleted), 0)::BIGINT AS transcript_count,
    COALESCE((SELECT COUNT(*) FROM summaries s WHERE s.meeting_id = m.id), 0)::BIGINT AS summary_count,
    COALESCE((SELECT COUNT(*) FROM artifacts a WHERE a.meeting_id = m.id), 0)::BIGINT AS artifact_count,
    COALESCE((SELECT SUM(a.size_bytes) FROM artifacts a WHERE a.meeting_id = m.id), 0)::BIGINT AS artifact_bytes,
    COALESCE((SELECT SUM(a.size_bytes) FROM artifacts a WHERE a.meeting_id = m.id AND a.kind IN ('raw_audio', 'audio', 'mixdown_audio', 'speaker_audio')), 0)::BIGINT AS raw_audio_artifact_bytes,
    COALESCE((SELECT SUM(a.size_bytes) FROM artifacts a WHERE a.meeting_id = m.id AND a.kind IN ('transcript', 'masked_transcript')), 0)::BIGINT AS transcript_artifact_bytes,
    COALESCE((SELECT SUM(a.size_bytes) FROM artifacts a WHERE a.meeting_id = m.id AND a.kind IN ('summary', 'summary_markdown')), 0)::BIGINT AS summary_artifact_bytes,
    COALESCE((SELECT SUM(a.size_bytes) FROM artifacts a WHERE a.meeting_id = m.id AND a.kind IN ('debug', 'debug_artifact', 'whisper_debug')), 0)::BIGINT AS debug_artifact_bytes,
    COALESCE((SELECT COUNT(*) FROM usage_events u WHERE u.meeting_id = m.id), 0)::BIGINT AS usage_event_count,
    COALESCE((SELECT COUNT(*) FROM audit_events e WHERE e.resource_id = m.id), 0)::BIGINT AS audit_event_count
FROM meetings m
WHERE m.id = $1
  AND m.guild_id = $2
"#;

pub const ADMIN_RETENTION_MARK_MEETING_TRANSCRIPTS_DELETED_SQL: &str = r#"
UPDATE transcripts
SET is_deleted = TRUE
WHERE meeting_id = $1
  AND EXISTS (
    SELECT 1
    FROM meetings m
    WHERE m.id = $1
      AND m.guild_id = $2
      AND m.status IN ('posted', 'failed', 'aborted')
  )
  AND is_deleted = FALSE
"#;

pub const ADMIN_RETENTION_MARK_RAW_WORKSPACE_CLEANED_SQL: &str = r#"
UPDATE meetings
SET retention_raw_cleaned_at=NOW(),
    updated_at=NOW()
WHERE id=$1
  AND guild_id=$2
  AND status IN ('posted', 'failed', 'aborted')
  AND retention_raw_cleaned_at IS NULL
"#;

pub const ADMIN_RETENTION_DELETE_MEETING_SUMMARIES_SQL: &str = r#"
WITH cleared_summary_title AS (
  UPDATE meetings m
  SET title=NULL,
      updated_at=NOW()
  WHERE m.id = $1
    AND m.guild_id = $2
    AND m.title IS NOT NULL
    AND m.status IN ('posted', 'failed', 'aborted')
  RETURNING m.id
)
DELETE FROM summaries
WHERE meeting_id = $1
  AND EXISTS (
    SELECT 1
    FROM meetings m
    WHERE m.id = $1
      AND m.guild_id = $2
      AND m.status IN ('posted', 'failed', 'aborted')
      AND (
        m.title IS NULL
        OR m.id IN (SELECT id FROM cleared_summary_title)
      )
  )
"#;

pub const ADMIN_RETENTION_DELETE_MEETING_ARTIFACTS_BY_KIND_SQL: &str = r#"
DELETE FROM artifacts
WHERE meeting_id = $1
  AND EXISTS (
    SELECT 1
    FROM meetings m
    WHERE m.id = $1
      AND m.guild_id = $2
      AND m.status IN ('posted', 'failed', 'aborted')
  )
  AND (
    ($3::boolean AND kind IN ('raw_audio', 'audio', 'mixdown_audio', 'speaker_audio'))
    OR ($4::boolean AND kind IN ('transcript', 'masked_transcript'))
    OR ($5::boolean AND kind IN ('summary', 'summary_markdown'))
    OR ($6::boolean AND kind IN ('debug', 'debug_artifact', 'whisper_debug'))
  )
"#;

pub const ADMIN_RETENTION_EXPIRED_RAW_WORKSPACES_SQL: &str = r#"
SELECT id, guild_id, voice_channel_id
FROM meetings
WHERE guild_id = $2
  AND stopped_at IS NOT NULL
  AND stopped_at < NOW() - (($1 || ' days')::interval)
  AND status IN ('posted', 'failed', 'aborted')
  AND retention_raw_cleaned_at IS NULL
"#;

pub const ADMIN_RETENTION_EXPIRED_DEBUG_WORKSPACES_SQL: &str = r#"
SELECT id, guild_id, voice_channel_id
FROM meetings
WHERE guild_id = $2
  AND stopped_at IS NOT NULL
  AND stopped_at < NOW() - (($1 || ' days')::interval)
  AND status IN ('posted', 'failed', 'aborted')
  AND retention_raw_cleaned_at IS NOT NULL
"#;

pub const ADMIN_RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL: &str = r#"
SELECT m.id, m.guild_id, m.voice_channel_id
FROM meetings m
WHERE m.guild_id = $2
  AND m.status IN ('posted', 'failed', 'aborted')
  AND (
    NOT EXISTS (
      SELECT 1
      FROM transcripts active_t
      WHERE active_t.meeting_id = m.id
        AND active_t.is_deleted = FALSE
        AND active_t.created_at >= NOW() - (($1 || ' days')::interval)
    )
  )
  AND (
    EXISTS (
      SELECT 1
      FROM transcripts expired_t
      WHERE expired_t.meeting_id = m.id
        AND expired_t.created_at < NOW() - (($1 || ' days')::interval)
    )
    OR (m.stopped_at IS NOT NULL AND m.stopped_at < NOW() - (($1 || ' days')::interval))
  )
"#;

pub const ADMIN_RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL: &str = r#"
SELECT m.id, m.guild_id, m.voice_channel_id
FROM meetings m
WHERE m.guild_id = $2
  AND m.status IN ('posted', 'failed', 'aborted')
  AND NOT EXISTS (
    SELECT 1
    FROM summaries active_s
    WHERE active_s.meeting_id = m.id
      AND active_s.created_at >= NOW() - (($1 || ' days')::interval)
  )
  AND (
    (
      m.stopped_at IS NOT NULL
      AND m.stopped_at < NOW() - (($1 || ' days')::interval)
    )
    OR EXISTS (
    SELECT 1
    FROM summaries expired_s
    WHERE expired_s.meeting_id = m.id
      AND expired_s.created_at < NOW() - (($1 || ' days')::interval)
    )
  )
"#;

pub const ADMIN_RETENTION_EXPIRED_ARTIFACTS_PREVIEW_SQL: &str = r#"
SELECT
    COUNT(*)::BIGINT AS artifact_count,
    COALESCE(SUM(a.size_bytes), 0)::BIGINT AS artifact_bytes
FROM artifacts a
JOIN meetings m ON m.id = a.meeting_id
WHERE m.guild_id = $1
  AND a.expires_at IS NOT NULL
  AND a.expires_at <= NOW()
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const ADMIN_RETENTION_DEBUG_ARTIFACTS_PREVIEW_SQL: &str = r#"
SELECT COUNT(*)::BIGINT AS artifact_count
FROM artifacts a
JOIN meetings m ON m.id = a.meeting_id
WHERE m.guild_id = $2
  AND a.kind IN ('debug', 'debug_artifact', 'whisper_debug')
  AND m.stopped_at IS NOT NULL
  AND m.stopped_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const ADMIN_RETENTION_MARK_TRANSCRIPTS_DELETED_SQL: &str = r#"
UPDATE transcripts t
SET is_deleted=TRUE
FROM meetings m
WHERE t.meeting_id = m.id
  AND m.guild_id = $2
  AND t.is_deleted=FALSE
  AND t.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const ADMIN_RETENTION_DELETE_SUMMARIES_SQL: &str = r#"
WITH expired_summary_meetings AS (
  SELECT m.id
  FROM meetings m
  WHERE m.guild_id = $2
    AND m.status IN ('posted', 'failed', 'aborted')
    AND NOT EXISTS (
      SELECT 1
      FROM summaries active_s
      WHERE active_s.meeting_id = m.id
        AND active_s.created_at >= NOW() - (($1 || ' days')::interval)
    )
    AND (
      (
        m.stopped_at IS NOT NULL
        AND m.stopped_at < NOW() - (($1 || ' days')::interval)
      )
      OR EXISTS (
        SELECT 1
        FROM summaries expired_s
        WHERE expired_s.meeting_id = m.id
          AND expired_s.created_at < NOW() - (($1 || ' days')::interval)
      )
    )
),
cleared_summary_titles AS (
  UPDATE meetings m
  SET title=NULL,
      updated_at=NOW()
  WHERE m.title IS NOT NULL
    AND m.guild_id = $2
    AND m.id IN (SELECT id FROM expired_summary_meetings)
  RETURNING m.id
)
DELETE FROM summaries s
USING meetings m
WHERE s.meeting_id = m.id
  AND m.guild_id = $2
  AND s.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
  AND (
    m.id NOT IN (SELECT id FROM expired_summary_meetings)
    OR m.id IN (SELECT id FROM cleared_summary_titles)
    OR m.title IS NULL
  )
"#;

pub const ADMIN_RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND m.guild_id = $1
  AND a.expires_at IS NOT NULL
  AND a.expires_at <= NOW()
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const ADMIN_RETENTION_DELETE_RAW_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND m.guild_id = $2
  AND a.kind IN ('raw_audio', 'audio', 'mixdown_audio', 'speaker_audio')
  AND m.stopped_at IS NOT NULL
  AND m.stopped_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const ADMIN_RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND m.guild_id = $2
  AND a.kind IN ('transcript', 'masked_transcript')
  AND a.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const ADMIN_RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND m.guild_id = $2
  AND a.kind IN ('summary', 'summary_markdown')
  AND a.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const ADMIN_RETENTION_DELETE_DEBUG_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND m.guild_id = $2
  AND a.kind IN ('debug', 'debug_artifact', 'whisper_debug')
  AND m.stopped_at IS NOT NULL
  AND m.stopped_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const LIST_RECENT_USAGE_EVENTS_SQL: &str = r#"
SELECT id, tenant_id, guild_id, meeting_id, job_id, resource_type, resource_id,
       metric, quantity, detail_json::TEXT,
       to_char(observed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS observed_at,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at
FROM usage_events
WHERE (
    NULLIF($1, '') IS NULL
    OR tenant_id = NULLIF($1, '')
    OR (
        tenant_id IS NULL
        AND guild_id IN (
            SELECT guild_id
            FROM tenant_discord_guilds
            WHERE tenant_id = NULLIF($1, '')
              AND status = 'active'
        )
    )
)
  AND (NULLIF($2, '') IS NULL OR guild_id = NULLIF($2, ''))
ORDER BY observed_at DESC, id DESC
LIMIT $3::TEXT::INTEGER
"#;

pub const AGGREGATE_RECENT_USAGE_SQL: &str = r#"
SELECT metric, COALESCE(SUM(quantity), 0)::TEXT AS quantity
FROM usage_events
WHERE (
    NULLIF($1, '') IS NULL
    OR tenant_id = NULLIF($1, '')
    OR (
        tenant_id IS NULL
        AND guild_id IN (
            SELECT guild_id
            FROM tenant_discord_guilds
            WHERE tenant_id = NULLIF($1, '')
              AND status = 'active'
        )
    )
)
  AND (NULLIF($2, '') IS NULL OR guild_id = NULLIF($2, ''))
  AND observed_at >= NOW() - make_interval(secs => $3::TEXT::BIGINT)
GROUP BY metric
ORDER BY metric
"#;

pub const RESOLVE_PLAN_FOR_GUILD_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $1
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
), active_assignment AS (
    SELECT gpa.id AS assignment_id,
           gpa.tenant_id,
           gpa.guild_id,
           gpa.plan_id,
           gpa.source AS assignment_source,
           gpa.period_anchor,
           gpa.valid_from,
           gpa.valid_until
    FROM guild_plan_assignments gpa
    JOIN plans p ON p.id = gpa.plan_id
    WHERE gpa.guild_id = $1
      AND gpa.status = 'active'
      AND p.status IN ('active', 'archived')
      AND gpa.valid_from <= $2::TEXT::TIMESTAMPTZ
      AND (gpa.valid_until IS NULL OR gpa.valid_until > $2::TEXT::TIMESTAMPTZ)
      AND gpa.tenant_id = (SELECT tenant_id FROM active_tenant)
    ORDER BY gpa.valid_from DESC, gpa.created_at DESC, gpa.id DESC
    LIMIT 1
), candidates AS (
    SELECT aa.assignment_id,
           aa.tenant_id,
           aa.guild_id,
           p.id AS plan_id,
           p.code AS plan_code,
           p.name AS plan_name,
           p.kind AS plan_kind,
           'assignment' AS resolution_source,
           aa.assignment_source,
           aa.period_anchor,
           aa.valid_from,
           aa.valid_until,
           0 AS priority
    FROM active_assignment aa
    JOIN plans p ON p.id = aa.plan_id
    UNION ALL
    SELECT NULL AS assignment_id,
           (SELECT tenant_id FROM active_tenant) AS tenant_id,
           $1 AS guild_id,
           p.id AS plan_id,
           p.code AS plan_code,
           p.name AS plan_name,
           p.kind AS plan_kind,
           'fallback' AS resolution_source,
           NULL AS assignment_source,
           NULL AS period_anchor,
           NULL AS valid_from,
           NULL AS valid_until,
           1 AS priority
    FROM plans p
    WHERE p.code = $3
      AND p.status = 'active'
      AND EXISTS (SELECT 1 FROM active_tenant)
    UNION ALL
    SELECT NULL AS assignment_id,
           (SELECT tenant_id FROM active_tenant) AS tenant_id,
           $1 AS guild_id,
           p.id AS plan_id,
           p.code AS plan_code,
           p.name AS plan_name,
           p.kind AS plan_kind,
           'fallback' AS resolution_source,
           NULL AS assignment_source,
           NULL AS period_anchor,
           NULL AS valid_from,
           NULL AS valid_until,
           2 AS priority
    FROM plans p
    WHERE p.code = 'default'
      AND p.status = 'active'
      AND $3 <> 'default'
      AND EXISTS (SELECT 1 FROM active_tenant)
), selected_plan AS (
    SELECT *
    FROM candidates
    ORDER BY priority
    LIMIT 1
)
SELECT sp.assignment_id,
       sp.tenant_id,
       sp.guild_id,
       sp.plan_id,
       sp.plan_code,
       sp.plan_name,
       sp.plan_kind,
       sp.resolution_source,
       sp.assignment_source,
       to_char(sp.period_anchor AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS period_anchor,
       to_char(sp.valid_from AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_from,
       to_char(sp.valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_until,
       pq.id AS quota_id,
       pq.dimension,
       pq.period,
       pq.limit_value::TEXT,
       pq.unlimited::TEXT,
       pq.enforcement_mode
FROM selected_plan sp
LEFT JOIN plan_quotas pq ON pq.plan_id = sp.plan_id
ORDER BY pq.dimension, pq.period, pq.id
"#;

pub const LIST_ADMIN_PLANS_SQL: &str = r#"
SELECT p.id,
       p.code,
       p.name,
       p.kind,
       p.status,
       to_char(p.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(p.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
       pq.id AS quota_id,
       pq.plan_id AS quota_plan_id,
       pq.dimension AS quota_dimension,
       pq.period AS quota_period,
       pq.limit_value AS quota_limit_value,
       pq.unlimited AS quota_unlimited,
       pq.enforcement_mode AS quota_enforcement_mode,
       to_char(pq.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS quota_created_at,
       to_char(pq.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS quota_updated_at
FROM plans p
LEFT JOIN plan_quotas pq ON pq.plan_id = p.id
ORDER BY p.kind, p.code, p.id, pq.dimension, pq.period, pq.id
"#;

pub const GET_ADMIN_PLAN_SQL: &str = r#"
SELECT p.id,
       p.code,
       p.name,
       p.kind,
       p.status,
       to_char(p.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(p.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
       pq.id AS quota_id,
       pq.plan_id AS quota_plan_id,
       pq.dimension AS quota_dimension,
       pq.period AS quota_period,
       pq.limit_value AS quota_limit_value,
       pq.unlimited AS quota_unlimited,
       pq.enforcement_mode AS quota_enforcement_mode,
       to_char(pq.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS quota_created_at,
       to_char(pq.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS quota_updated_at
FROM plans p
LEFT JOIN plan_quotas pq ON pq.plan_id = p.id
WHERE p.id = $1
ORDER BY pq.dimension, pq.period, pq.id
"#;

pub const GET_ADMIN_PLAN_BY_CODE_SQL: &str = r#"
SELECT p.id,
       p.code,
       p.name,
       p.kind,
       p.status,
       to_char(p.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(p.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
       pq.id AS quota_id,
       pq.plan_id AS quota_plan_id,
       pq.dimension AS quota_dimension,
       pq.period AS quota_period,
       pq.limit_value AS quota_limit_value,
       pq.unlimited AS quota_unlimited,
       pq.enforcement_mode AS quota_enforcement_mode,
       to_char(pq.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS quota_created_at,
       to_char(pq.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS quota_updated_at
FROM plans p
LEFT JOIN plan_quotas pq ON pq.plan_id = p.id
WHERE p.code = $1
ORDER BY pq.dimension, pq.period, pq.id
"#;

pub const INSERT_ADMIN_PLAN_SQL: &str = r#"
INSERT INTO plans (id, code, name, kind, status, created_at, updated_at)
VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
RETURNING id
"#;

pub const UPDATE_ADMIN_PLAN_SQL: &str = r#"
UPDATE plans
SET code = $2,
    name = $3,
    kind = $4,
    status = COALESCE(NULLIF($5, ''), status),
    updated_at = NOW()
WHERE id = $1
RETURNING id
"#;

pub const ARCHIVE_ADMIN_PLAN_SQL: &str = r#"
UPDATE plans
SET status = 'archived',
    updated_at = CASE WHEN status <> 'archived' THEN NOW() ELSE updated_at END
WHERE id = $1
RETURNING id
"#;

pub const LIST_ADMIN_PLAN_QUOTAS_SQL: &str = r#"
SELECT id,
       plan_id,
       dimension,
       period,
       limit_value,
       unlimited,
       enforcement_mode,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM plan_quotas
WHERE plan_id = $1
ORDER BY dimension, period, id
"#;

pub const GET_ADMIN_PLAN_QUOTA_SQL: &str = r#"
SELECT id,
       plan_id,
       dimension,
       period,
       limit_value,
       unlimited,
       enforcement_mode,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM plan_quotas
WHERE id = $1
"#;

pub const INSERT_ADMIN_PLAN_QUOTA_SQL: &str = r#"
INSERT INTO plan_quotas (
    id, plan_id, dimension, period, limit_value, unlimited, enforcement_mode, created_at, updated_at
)
SELECT $1, p.id, $3, $4, $5, $6, $7, NOW(), NOW()
FROM plans p
WHERE p.id = $2
RETURNING id,
          plan_id,
          dimension,
          period,
          limit_value,
          unlimited,
          enforcement_mode,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const UPDATE_ADMIN_PLAN_QUOTA_SQL: &str = r#"
UPDATE plan_quotas
SET dimension = $2,
    period = $3,
    limit_value = $4,
    unlimited = $5,
    enforcement_mode = $6,
    updated_at = NOW()
WHERE id = $1
RETURNING id,
          plan_id,
          dimension,
          period,
          limit_value,
          unlimited,
          enforcement_mode,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const DELETE_ADMIN_PLAN_QUOTA_SQL: &str = r#"
DELETE FROM plan_quotas
WHERE id = $1
RETURNING id,
          plan_id,
          dimension,
          period,
          limit_value,
          unlimited,
          enforcement_mode,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const LIST_ADMIN_GUILD_PLAN_ASSIGNMENTS_SQL: &str = r#"
SELECT gpa.id,
       gpa.tenant_id,
       gpa.guild_id,
       gpa.plan_id,
       p.code AS plan_code,
       p.name AS plan_name,
       gpa.status,
       to_char(gpa.valid_from AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_from,
       to_char(gpa.valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_until,
       to_char(gpa.period_anchor AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS period_anchor,
       gpa.assigned_by_user_id,
       gpa.source,
       to_char(gpa.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(gpa.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM guild_plan_assignments gpa
JOIN plans p ON p.id = gpa.plan_id
WHERE (NULLIF($1, '') IS NULL OR gpa.guild_id = NULLIF($1, ''))
  AND (NULLIF($2, '') IS NULL OR gpa.tenant_id = NULLIF($2, ''))
  AND ($3 OR gpa.status = 'active')
ORDER BY gpa.valid_from DESC, gpa.created_at DESC, gpa.id DESC
LIMIT $4::INTEGER
"#;

pub const GET_ADMIN_GUILD_PLAN_ASSIGNMENT_SQL: &str = r#"
SELECT gpa.id,
       gpa.tenant_id,
       gpa.guild_id,
       gpa.plan_id,
       p.code AS plan_code,
       p.name AS plan_name,
       gpa.status,
       to_char(gpa.valid_from AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_from,
       to_char(gpa.valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_until,
       to_char(gpa.period_anchor AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS period_anchor,
       gpa.assigned_by_user_id,
       gpa.source,
       to_char(gpa.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(gpa.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM guild_plan_assignments gpa
JOIN plans p ON p.id = gpa.plan_id
WHERE gpa.id = $1
"#;

pub const INSERT_ADMIN_GUILD_PLAN_ASSIGNMENT_SQL: &str = r#"
WITH input AS (
    SELECT $1 AS id,
           $2 AS tenant_id,
           $3 AS guild_id,
           $4 AS plan_id,
           $5::TIMESTAMPTZ AS valid_from,
           NULLIF($6, '')::TIMESTAMPTZ AS valid_until,
           NULLIF($7, '') AS assigned_by_user_id,
           $8 AS source
), valid_input AS (
    SELECT input.*
    FROM input
    JOIN tenant_discord_guilds tg
      ON tg.tenant_id = input.tenant_id
     AND tg.guild_id = input.guild_id
     AND tg.status = 'active'
    JOIN plans p
      ON p.id = input.plan_id
     AND p.status = 'active'
), tenant_period AS (
    UPDATE tenants
    SET period_anchor = COALESCE(
            period_anchor,
            date_trunc('day', (SELECT valid_from FROM valid_input) AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
        ),
        updated_at = CASE WHEN period_anchor IS NULL THEN NOW() ELSE updated_at END
    WHERE id = (SELECT tenant_id FROM valid_input)
      AND status = 'active'
    RETURNING id, period_anchor
), inserted AS (
    INSERT INTO guild_plan_assignments (
        id, tenant_id, guild_id, plan_id, status, valid_from, valid_until,
        period_anchor, assigned_by_user_id, source, created_at, updated_at
    )
    SELECT valid_input.id,
           valid_input.tenant_id,
           valid_input.guild_id,
           valid_input.plan_id,
           'active',
           valid_input.valid_from,
           valid_input.valid_until,
           tenant_period.period_anchor,
           valid_input.assigned_by_user_id,
           valid_input.source,
           NOW(),
           NOW()
    FROM valid_input
    JOIN tenant_period ON tenant_period.id = valid_input.tenant_id
    RETURNING id,
              tenant_id,
              guild_id,
              plan_id,
              status,
              valid_from,
              valid_until,
              period_anchor,
              assigned_by_user_id,
              source,
              created_at,
              updated_at
)
SELECT inserted.id,
       inserted.tenant_id,
       inserted.guild_id,
       inserted.plan_id,
       p.code AS plan_code,
       p.name AS plan_name,
       inserted.status,
       to_char(inserted.valid_from AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_from,
       to_char(inserted.valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_until,
       to_char(inserted.period_anchor AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS period_anchor,
       inserted.assigned_by_user_id,
       inserted.source,
       to_char(inserted.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(inserted.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM inserted
JOIN plans p ON p.id = inserted.plan_id
"#;

pub const UPDATE_ADMIN_GUILD_PLAN_ASSIGNMENT_SQL: &str = r#"
WITH updated AS (
    UPDATE guild_plan_assignments gpa
    SET plan_id = $2,
        valid_from = $3::TIMESTAMPTZ,
        valid_until = NULLIF($4, '')::TIMESTAMPTZ,
        assigned_by_user_id = NULLIF($5, ''),
        source = $6,
        updated_at = NOW()
    FROM plans p
    WHERE gpa.id = $1
      AND gpa.status = 'active'
      AND p.id = $2
      AND p.status = 'active'
    RETURNING gpa.id,
              gpa.tenant_id,
              gpa.guild_id,
              gpa.plan_id,
              gpa.status,
              gpa.valid_from,
              gpa.valid_until,
              gpa.period_anchor,
              gpa.assigned_by_user_id,
              gpa.source,
              gpa.created_at,
              gpa.updated_at
)
SELECT updated.id,
       updated.tenant_id,
       updated.guild_id,
       updated.plan_id,
       p.code AS plan_code,
       p.name AS plan_name,
       updated.status,
       to_char(updated.valid_from AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_from,
       to_char(updated.valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_until,
       to_char(updated.period_anchor AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS period_anchor,
       updated.assigned_by_user_id,
       updated.source,
       to_char(updated.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM updated
JOIN plans p ON p.id = updated.plan_id
"#;

pub const ARCHIVE_ADMIN_GUILD_PLAN_ASSIGNMENT_SQL: &str = r#"
WITH archived AS (
    UPDATE guild_plan_assignments
    SET status = 'revoked',
        valid_until = CASE
            WHEN status = 'active'
             AND valid_from <= NOW()
             AND valid_until IS NULL THEN NOW()
            ELSE valid_until
        END,
        updated_at = CASE WHEN status = 'active' THEN NOW() ELSE updated_at END
    WHERE id = $1
    RETURNING id,
              tenant_id,
              guild_id,
              plan_id,
              status,
              valid_from,
              valid_until,
              period_anchor,
              assigned_by_user_id,
              source,
              created_at,
              updated_at
)
SELECT archived.id,
       archived.tenant_id,
       archived.guild_id,
       archived.plan_id,
       p.code AS plan_code,
       p.name AS plan_name,
       archived.status,
       to_char(archived.valid_from AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_from,
       to_char(archived.valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS valid_until,
       to_char(archived.period_anchor AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS period_anchor,
       archived.assigned_by_user_id,
       archived.source,
       to_char(archived.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(archived.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM archived
JOIN plans p ON p.id = archived.plan_id
"#;

pub const SET_MEETING_STATUS_CAS_SQL: &str = r#"
WITH updated AS (
    UPDATE meetings
    SET status=$1, updated_at=NOW()
    WHERE id=$2 AND status=$3
    RETURNING 1
), existing AS (
    SELECT 1 FROM meetings WHERE id=$2
)
SELECT CASE
    WHEN EXISTS (SELECT 1 FROM updated) THEN 'updated'
    WHEN EXISTS (SELECT 1 FROM existing) THEN 'conflict'
    ELSE 'not_found'
END
"#;

pub const RECOVERY_SCAN_SQL: &str = r#"
SELECT id, status, voice_channel_id
FROM meetings
WHERE status IN ('recording', 'stopping', 'transcribing', 'summarizing')
"#;

pub const RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL: &str = r#"
UPDATE jobs
SET status='queued',
    error_message=NULL,
    next_run_at=NULL,
    leased_until=NULL,
    claim_token=NULL,
    updated_at=NOW()
WHERE id=$1
  AND job_type='summarize'
  AND status='running'
  AND (
    (leased_until IS NOT NULL AND leased_until < NOW())
    OR (
      leased_until IS NULL
      AND updated_at < NOW() - INTERVAL '15 minutes'
    )
  )
"#;

pub const HEARTBEAT_RUNNING_JOB_SQL: &str = r#"
UPDATE jobs
SET leased_until = NOW() + INTERVAL '90 seconds',
    updated_at = NOW()
WHERE id = $1
  AND claim_token = $2
  AND status = 'running'
"#;

pub const RECOVERY_SUMMARY_JOB_STATUS_SQL: &str = r#"
SELECT status,
       to_char(next_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS next_run_at
FROM jobs
WHERE id=$1
  AND job_type='summarize'
LIMIT 1
"#;

pub const RECOVERY_READY_SUMMARY_JOBS_SQL: &str = r#"
WITH requeued AS (
    UPDATE jobs
    SET status='queued',
        error_message=NULL,
        next_run_at=NULL,
        leased_until=NULL,
        claim_token=NULL,
        updated_at=NOW()
    WHERE id IN (
        SELECT id
        FROM jobs
        WHERE job_type='summarize'
          AND status='running'
          AND (
            (leased_until IS NOT NULL AND leased_until < NOW())
            OR (
              leased_until IS NULL
              AND updated_at < NOW() - INTERVAL '15 minutes'
            )
          )
        ORDER BY updated_at, id
        LIMIT 25
        FOR UPDATE SKIP LOCKED
    )
    RETURNING meeting_id
)
SELECT DISTINCT meeting_id
FROM (
    SELECT meeting_id FROM requeued
    UNION ALL
    SELECT meeting_id
    FROM jobs
    WHERE job_type='summarize'
      AND status='queued'
      AND (next_run_at IS NULL OR next_run_at <= NOW())
) ready
ORDER BY meeting_id
LIMIT 25
"#;

pub const ENQUEUE_JOB_SQL: &str = r#"
INSERT INTO jobs (id, meeting_id, job_type, status, retry_count, created_at, updated_at)
VALUES ($1, $2, $3, 'queued', 0, NOW(), NOW())
"#;

pub const CLAIM_JOB_SQL: &str = r#"
UPDATE jobs
SET status = 'running',
    claim_token = $2,
    leased_until = NOW() + INTERVAL '90 seconds',
    next_run_at = NULL,
    updated_at = NOW()
WHERE id = (
    SELECT id
    FROM jobs
    WHERE job_type = $1
      AND status = 'queued'
      AND (next_run_at IS NULL OR next_run_at <= NOW())
    ORDER BY COALESCE(next_run_at, created_at) ASC, created_at ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
RETURNING id, meeting_id, job_type, status, retry_count, error_message,
          claim_token,
          to_char(next_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS next_run_at
"#;

pub const CLAIM_JOB_BY_ID_SQL: &str = r#"
UPDATE jobs
SET status = 'running',
    claim_token = $2,
    leased_until = NOW() + INTERVAL '90 seconds',
    next_run_at = NULL,
    updated_at = NOW()
WHERE id = $1
  AND status = 'queued'
  AND (next_run_at IS NULL OR next_run_at <= NOW())
RETURNING id, meeting_id, job_type, status, retry_count, error_message,
          claim_token,
          to_char(next_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS next_run_at
"#;

pub const MARK_JOB_DONE_SQL: &str = r#"
UPDATE jobs
SET status = 'done',
    error_message = NULL,
    next_run_at = NULL,
    leased_until = NULL,
    claim_token = NULL,
    finished_at = NOW(),
    updated_at = NOW()
WHERE id = $1
  AND claim_token = $2
  AND status = 'running'
"#;

pub const MARK_JOB_FAILED_SQL: &str = r#"
UPDATE jobs
SET status = 'failed',
    error_message = $2,
    next_run_at = NULL,
    leased_until = NULL,
    claim_token = NULL,
    finished_at = NOW(),
    dead_lettered_at = NOW(),
    updated_at = NOW()
WHERE id = $1
  AND claim_token = $3
  AND status = 'running'
"#;

pub const RETRY_JOB_SQL: &str = r#"
UPDATE jobs
SET
  status = CASE WHEN retry_count + 1 > $3::integer THEN 'failed' ELSE 'queued' END,
  retry_count = retry_count + 1,
  error_message = $2,
  next_run_at = CASE
    WHEN retry_count + 1 > $3::integer THEN NULL
    ELSE NOW() + make_interval(secs => LEAST(900, 30 * POWER(2, LEAST(retry_count, 5))::integer))
  END,
  leased_until = NULL,
  claim_token = NULL,
  finished_at = CASE WHEN retry_count + 1 > $3::integer THEN NOW() ELSE NULL END,
  dead_lettered_at = CASE WHEN retry_count + 1 > $3::integer THEN NOW() ELSE NULL END,
  canceled_at = NULL,
  cancel_reason = NULL,
  updated_at = NOW()
WHERE id = $1
  AND claim_token = $4
  AND status = 'running'
RETURNING status
"#;

pub const LIST_GUILD_JOBS_SQL: &str = r#"
SELECT j.id,
       j.meeting_id,
       m.guild_id,
       j.job_type,
       j.status,
       j.retry_count,
       j.error_message,
       to_char(j.next_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS next_run_at,
       to_char(j.leased_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS leased_until,
       to_char(j.finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS finished_at,
       to_char(j.dead_lettered_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS dead_lettered_at,
       to_char(j.canceled_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS canceled_at,
       j.cancel_reason,
       to_char(j.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(j.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM jobs j
JOIN meetings m ON m.id = j.meeting_id
WHERE m.guild_id = $1
  AND (NULLIF($2, '') IS NULL OR j.status = NULLIF($2, ''))
  AND (NULLIF($3, '') IS NULL OR j.job_type = NULLIF($3, ''))
ORDER BY j.updated_at DESC, j.created_at DESC, j.id DESC
LIMIT $4::integer
"#;

pub const ADMIN_RETRY_JOB_SQL: &str = r#"
UPDATE jobs j
SET status = 'queued',
    retry_count = 0,
    error_message = NULL,
    next_run_at = COALESCE(NULLIF($3, '')::timestamptz, NOW()),
    leased_until = NULL,
    claim_token = NULL,
    finished_at = NULL,
    dead_lettered_at = NULL,
    canceled_at = NULL,
    cancel_reason = NULL,
    updated_at = NOW()
FROM meetings m
WHERE m.id = j.meeting_id
  AND m.guild_id = $2
  AND j.id = $1
  AND j.status IN ('failed', 'canceled')
RETURNING j.id,
          j.meeting_id,
          $2::text AS guild_id,
          j.job_type,
          j.status,
          j.retry_count,
          j.error_message,
          to_char(j.next_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS next_run_at,
          to_char(j.leased_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS leased_until,
          to_char(j.finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS finished_at,
          to_char(j.dead_lettered_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS dead_lettered_at,
          to_char(j.canceled_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS canceled_at,
          j.cancel_reason,
          to_char(j.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(j.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const ADMIN_CANCEL_JOB_SQL: &str = r#"
UPDATE jobs j
SET status = 'canceled',
    error_message = NULL,
    next_run_at = NULL,
    leased_until = NULL,
    claim_token = NULL,
    finished_at = NOW(),
    dead_lettered_at = NULL,
    canceled_at = NOW(),
    cancel_reason = $3,
    updated_at = NOW()
FROM meetings m
WHERE m.id = j.meeting_id
  AND m.guild_id = $2
  AND j.id = $1
  AND j.status = 'queued'
RETURNING j.id,
          j.meeting_id,
          $2::text AS guild_id,
          j.job_type,
          j.status,
          j.retry_count,
          j.error_message,
          to_char(j.next_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS next_run_at,
          to_char(j.leased_until AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS leased_until,
          to_char(j.finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS finished_at,
          to_char(j.dead_lettered_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS dead_lettered_at,
          to_char(j.canceled_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS canceled_at,
          j.cancel_reason,
          to_char(j.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(j.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const INSERT_SUMMARY_SQL: &str = r#"
INSERT INTO summaries (id, meeting_id, version, markdown)
VALUES ($1, $2, 1, $3)
ON CONFLICT (meeting_id, version) DO UPDATE SET markdown = EXCLUDED.markdown
"#;

pub const UPSERT_MEETING_SPEAKER_SQL: &str = r#"
INSERT INTO meeting_speakers (meeting_id, speaker_id, username, nickname, display_name, updated_at)
VALUES ($1, $2, NULLIF($3, ''), NULLIF($4, ''), NULLIF($5, ''), NOW())
ON CONFLICT (meeting_id, speaker_id) DO UPDATE SET
    username = EXCLUDED.username,
    nickname = EXCLUDED.nickname,
    display_name = EXCLUDED.display_name,
    updated_at = NOW()
"#;

/// Build a multi-row INSERT statement for transcript segments.
/// Each segment uses 9 parameters with explicit type casts for the
/// String-only `SqlExecutor::execute` interface.
///
/// `param_offset` shifts placeholders by N positions to allow callers to
/// reserve leading parameters for surrounding statements.
pub fn build_insert_transcripts_sql_with_offset(count: usize, param_offset: usize) -> String {
    let mut sql = String::from(
        "INSERT INTO transcripts (id, meeting_id, speaker_id, start_ms, end_ms, text, confidence, is_noisy, source, is_deleted, transcript_stage, live_chunk_id) VALUES ",
    );
    for i in 0..count {
        let base = i * 9 + param_offset;
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!(
            "(${}, ${}, ${}, ${}::TEXT::INTEGER, ${}::TEXT::INTEGER, ${}, NULLIF(${},'')::TEXT::DOUBLE PRECISION, ${}::TEXT::BOOLEAN, ${}, FALSE, 'final', NULL)",
            base + 1,
            base + 2,
            base + 3,
            base + 4,
            base + 5,
            base + 6,
            base + 7,
            base + 8,
            base + 9,
        ));
    }
    sql.push_str(
        " ON CONFLICT (id) DO UPDATE SET \
        meeting_id = EXCLUDED.meeting_id, \
        speaker_id = EXCLUDED.speaker_id, \
        start_ms = EXCLUDED.start_ms, \
        end_ms = EXCLUDED.end_ms, \
        text = EXCLUDED.text, \
        confidence = EXCLUDED.confidence, \
        is_noisy = EXCLUDED.is_noisy, \
        source = EXCLUDED.source, \
        is_deleted = FALSE, \
        transcript_stage = 'final', \
        live_chunk_id = NULL",
    );
    sql
}

pub fn build_insert_transcripts_sql(count: usize) -> String {
    build_insert_transcripts_sql_with_offset(count, 0)
}

pub const UPSERT_GUILD_SETTINGS_SQL: &str = r#"
INSERT INTO guild_settings (
    guild_id, whisper_language, whisper_language_explicit, whisper_vad,
    auto_stop_grace_seconds, retention_raw_audio_ttl_days,
    retention_transcript_ttl_days, summary_enabled, updated_at
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
ON CONFLICT (guild_id) DO UPDATE SET
    whisper_language = EXCLUDED.whisper_language,
    whisper_language_explicit = EXCLUDED.whisper_language_explicit,
    whisper_vad = EXCLUDED.whisper_vad,
    auto_stop_grace_seconds = EXCLUDED.auto_stop_grace_seconds,
    retention_raw_audio_ttl_days = EXCLUDED.retention_raw_audio_ttl_days,
    retention_transcript_ttl_days = EXCLUDED.retention_transcript_ttl_days,
    summary_enabled = EXCLUDED.summary_enabled,
    updated_at = NOW()
"#;

pub const INSERT_AUDIT_EVENT_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = NULLIF($3, '')
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
),
candidate AS (
    SELECT
        $1 AS id,
        COALESCE(NULLIF($2, ''), (SELECT tenant_id FROM active_tenant)) AS tenant_id,
        NULLIF($3, '') AS guild_id,
        NULLIF($4, '') AS actor_user_id,
        $5 AS action,
        $6 AS resource_type,
        NULLIF($7, '') AS resource_id,
        COALESCE(NULLIF($8, '')::jsonb, '{}'::jsonb) AS request_metadata,
        COALESCE(NULLIF($9, '')::jsonb, '{}'::jsonb) AS detail_json,
        COALESCE(NULLIF($10, '')::timestamptz, NOW()) AS occurred_at
)
INSERT INTO audit_events (
    id, tenant_id, guild_id, actor_user_id, action, resource_type, resource_id,
    request_metadata, detail_json, occurred_at, created_at
) SELECT
    id,
    tenant_id,
    guild_id,
    actor_user_id,
    action,
    resource_type,
    resource_id,
    request_metadata,
    detail_json,
    occurred_at,
    NOW()
FROM candidate
ON CONFLICT (id) DO NOTHING
"#;

pub const PRUNE_STALE_AUDIT_EVENTS_SQL: &str = r#"
WITH stale_audit_events AS (
    SELECT events.ctid
    FROM audit_events events
    WHERE events.occurred_at < NOW() - INTERVAL '180 days'
    LIMIT 500
)
DELETE FROM audit_events events
USING stale_audit_events stale
WHERE events.ctid = stale.ctid
"#;

pub const LIST_RECENT_AUDIT_EVENTS_SQL: &str = r#"
SELECT id,
       tenant_id,
       guild_id,
       actor_user_id,
       action,
       resource_type,
       resource_id,
       request_metadata::text,
       detail_json::text,
       to_char(occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS occurred_at,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at
FROM audit_events
WHERE (NULLIF($1, '') IS NULL OR tenant_id = $1)
  AND (NULLIF($2, '') IS NULL OR guild_id = $2)
ORDER BY occurred_at DESC, created_at DESC, id DESC
LIMIT $3::integer
"#;

pub const GET_GUILD_SETTINGS_SQL: &str = r#"
SELECT whisper_language, whisper_language_explicit, whisper_vad,
       auto_stop_grace_seconds, retention_raw_audio_ttl_days,
       retention_transcript_ttl_days, summary_enabled,
       (
         bot_token_ciphertext IS NOT NULL
         AND bot_token_nonce IS NOT NULL
         AND bot_token_key_version IS NOT NULL
       ) AS discord_bot_token_registered,
       to_char(bot_token_updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS bot_token_updated_at,
       to_char(bot_token_last_validated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS bot_token_last_validated_at,
       bot_user_id, bot_username
FROM guild_settings
WHERE guild_id = $1
"#;

pub const GET_GUILD_SETTINGS_FOR_MEETING_SNAPSHOT_SQL: &str = r#"
SELECT whisper_language, whisper_language_explicit, whisper_vad,
       auto_stop_grace_seconds, retention_raw_audio_ttl_days,
       retention_transcript_ttl_days, summary_enabled
FROM guild_settings
WHERE guild_id = $1
"#;

pub const INSERT_RECORDING_MEETING_WITH_EFFECTIVE_SETTINGS_SQL: &str = r#"
WITH inserted_meeting AS (
    INSERT INTO meetings(
        id, guild_id, voice_channel_id, voice_channel_name, report_channel_id,
        status_message_channel_id, status_message_id, started_by_user_id, status
    )
    VALUES($1,$2,$3,NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,'recording')
    RETURNING id
)
INSERT INTO meeting_effective_settings (
    meeting_id, whisper_language, whisper_vad, whisper_beam_size,
    whisper_suppress_non_speech, whisper_prompt, whisper_temperature,
    whisper_resample_to_16k, auto_stop_grace_seconds, retention_raw_audio_ttl_days,
    retention_transcript_ttl_days, retention_summary_ttl_days, summary_enabled,
    summary_template_id, domain_knowledge_version_id, updated_at
)
SELECT id, NULLIF($9,''), $10::TEXT::BOOLEAN, $11::TEXT::INTEGER,
       $12::TEXT::BOOLEAN, NULLIF($13,''), $14::TEXT::DOUBLE PRECISION,
       $15::TEXT::BOOLEAN, $16::TEXT::BIGINT, $17::TEXT::INTEGER,
       $18::TEXT::INTEGER, NULLIF($19,'')::TEXT::INTEGER,
       $20::TEXT::BOOLEAN, NULLIF($21,''), NULLIF($22,''), NOW()
FROM inserted_meeting
"#;

pub const INSERT_SCHEDULED_MEETING_WITH_EFFECTIVE_SETTINGS_SQL: &str = r#"
WITH inserted_meeting AS (
    INSERT INTO meetings(
        id, guild_id, voice_channel_id, voice_channel_name, report_channel_id,
        status_message_channel_id, status_message_id, started_by_user_id, status
    )
    VALUES($1,$2,$3,NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,'scheduled')
    RETURNING id
)
INSERT INTO meeting_effective_settings (
    meeting_id, whisper_language, whisper_vad, whisper_beam_size,
    whisper_suppress_non_speech, whisper_prompt, whisper_temperature,
    whisper_resample_to_16k, auto_stop_grace_seconds, retention_raw_audio_ttl_days,
    retention_transcript_ttl_days, retention_summary_ttl_days, summary_enabled,
    summary_template_id, domain_knowledge_version_id, updated_at
)
SELECT id, NULLIF($9,''), $10::TEXT::BOOLEAN, $11::TEXT::INTEGER,
       $12::TEXT::BOOLEAN, NULLIF($13,''), $14::TEXT::DOUBLE PRECISION,
       $15::TEXT::BOOLEAN, $16::TEXT::BIGINT, $17::TEXT::INTEGER,
       $18::TEXT::INTEGER, NULLIF($19,'')::TEXT::INTEGER,
       $20::TEXT::BOOLEAN, NULLIF($21,''), NULLIF($22,''), NOW()
FROM inserted_meeting
"#;

pub const UPSERT_EFFECTIVE_MEETING_SETTINGS_SQL: &str = r#"
INSERT INTO meeting_effective_settings (
    meeting_id, whisper_language, whisper_vad, whisper_beam_size,
    whisper_suppress_non_speech, whisper_prompt, whisper_temperature,
    whisper_resample_to_16k, auto_stop_grace_seconds, retention_raw_audio_ttl_days,
    retention_transcript_ttl_days, retention_summary_ttl_days, summary_enabled,
    summary_template_id, domain_knowledge_version_id, updated_at
)
SELECT m.id, NULLIF($2,''), $3::TEXT::BOOLEAN, $4::TEXT::INTEGER,
       $5::TEXT::BOOLEAN, NULLIF($6,''), $7::TEXT::DOUBLE PRECISION,
       $8::TEXT::BOOLEAN, $9::TEXT::BIGINT, $10::TEXT::INTEGER,
       $11::TEXT::INTEGER, NULLIF($12,'')::TEXT::INTEGER,
       $13::TEXT::BOOLEAN, NULLIF($14,''), NULLIF($15,''), NOW()
FROM meetings m
WHERE m.id = $1
ON CONFLICT (meeting_id) DO UPDATE SET
    whisper_language = EXCLUDED.whisper_language,
    whisper_vad = EXCLUDED.whisper_vad,
    whisper_beam_size = EXCLUDED.whisper_beam_size,
    whisper_suppress_non_speech = EXCLUDED.whisper_suppress_non_speech,
    whisper_prompt = EXCLUDED.whisper_prompt,
    whisper_temperature = EXCLUDED.whisper_temperature,
    whisper_resample_to_16k = EXCLUDED.whisper_resample_to_16k,
    auto_stop_grace_seconds = EXCLUDED.auto_stop_grace_seconds,
    retention_raw_audio_ttl_days = EXCLUDED.retention_raw_audio_ttl_days,
    retention_transcript_ttl_days = EXCLUDED.retention_transcript_ttl_days,
    retention_summary_ttl_days = EXCLUDED.retention_summary_ttl_days,
    summary_enabled = EXCLUDED.summary_enabled,
    summary_template_id = EXCLUDED.summary_template_id,
    domain_knowledge_version_id = EXCLUDED.domain_knowledge_version_id,
    updated_at = NOW()
"#;

pub const GET_EFFECTIVE_MEETING_SETTINGS_SQL: &str = r#"
SELECT whisper_language, whisper_vad, whisper_beam_size,
       whisper_suppress_non_speech, whisper_prompt, whisper_temperature,
       whisper_resample_to_16k, auto_stop_grace_seconds,
       retention_raw_audio_ttl_days, retention_transcript_ttl_days,
       retention_summary_ttl_days, summary_enabled, summary_template_id,
       domain_knowledge_version_id
FROM meeting_effective_settings
WHERE meeting_id = $1
"#;

pub const GET_GUILD_BOT_TOKEN_SQL: &str = r#"
SELECT bot_token_ciphertext,
       bot_token_nonce,
       bot_token_key_version,
       to_char(bot_token_updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS bot_token_updated_at,
       to_char(bot_token_last_validated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS bot_token_last_validated_at,
       bot_user_id, bot_username
FROM guild_settings
WHERE guild_id = $1
  AND bot_token_ciphertext IS NOT NULL
  AND bot_token_nonce IS NOT NULL
  AND bot_token_key_version IS NOT NULL
"#;

pub const UPSERT_GUILD_BOT_TOKEN_SQL: &str = r#"
INSERT INTO guild_settings (
    guild_id, bot_token_ciphertext, bot_token_nonce, bot_token_key_version,
    bot_token_updated_at, bot_token_last_validated_at, bot_user_id, bot_username, updated_at
) VALUES ($1, $2, $3, $4, NOW(), NOW(), $5, $6, NOW())
ON CONFLICT (guild_id) DO UPDATE SET
    bot_token_ciphertext = EXCLUDED.bot_token_ciphertext,
    bot_token_nonce = EXCLUDED.bot_token_nonce,
    bot_token_key_version = EXCLUDED.bot_token_key_version,
    bot_token_updated_at = NOW(),
    bot_token_last_validated_at = NOW(),
    bot_user_id = EXCLUDED.bot_user_id,
    bot_username = EXCLUDED.bot_username,
    updated_at = NOW()
"#;

pub const CLEAR_GUILD_BOT_TOKEN_SQL: &str = r#"
UPDATE guild_settings
SET
    bot_token_ciphertext = NULL,
    bot_token_nonce = NULL,
    bot_token_key_version = NULL,
    bot_token_updated_at = NULL,
    bot_token_last_validated_at = NULL,
    bot_user_id = NULL,
    bot_username = NULL,
    updated_at = NOW()
WHERE guild_id = $1
"#;

pub const LIST_GUILD_MEETINGS_SQL: &str = r#"
SELECT id,
       status,
       to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as started_at,
       to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as stopped_at,
       meeting_duration_seconds,
       title,
       stop_reason,
       voice_channel_id,
       voice_channel_name
FROM meetings
WHERE guild_id = $1
  AND ($2::TEXT IS NULL OR voice_channel_id = $2)
ORDER BY started_at DESC
LIMIT $3 OFFSET $4
"#;

pub const COUNT_GUILD_MEETINGS_SQL: &str = r#"
SELECT count(*) FROM meetings
WHERE guild_id = $1
  AND ($2::TEXT IS NULL OR voice_channel_id = $2)
"#;

pub const LIST_VISIBLE_GUILD_MEETINGS_SQL: &str = r#"
SELECT id,
       status,
       to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as started_at,
       to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as stopped_at,
       meeting_duration_seconds,
       title,
       stop_reason,
       voice_channel_id,
       voice_channel_name
FROM meetings
WHERE guild_id = $1
  AND voice_channel_id = ANY($2::TEXT[])
  AND ($3::TEXT IS NULL OR voice_channel_id = $3)
ORDER BY started_at DESC
LIMIT $4 OFFSET $5
"#;

pub const COUNT_VISIBLE_GUILD_MEETINGS_SQL: &str = r#"
SELECT count(*) FROM meetings
WHERE guild_id = $1
  AND voice_channel_id = ANY($2::TEXT[])
  AND ($3::TEXT IS NULL OR voice_channel_id = $3)
"#;

pub const LIST_GUILD_MEETING_VOICE_CHANNELS_SQL: &str = r#"
SELECT voice_channel_id,
       (array_agg(voice_channel_name ORDER BY started_at DESC NULLS LAST, id DESC)
        FILTER (WHERE voice_channel_name IS NOT NULL AND voice_channel_name <> ''))[1]
       AS voice_channel_name
FROM meetings
WHERE guild_id = $1
GROUP BY voice_channel_id
ORDER BY MAX(started_at) DESC NULLS LAST, voice_channel_id ASC
LIMIT $2
"#;

pub const LIST_VISIBLE_GUILD_MEETING_VOICE_CHANNELS_SQL: &str = r#"
SELECT voice_channel_id,
       (array_agg(voice_channel_name ORDER BY started_at DESC NULLS LAST, id DESC)
        FILTER (WHERE voice_channel_name IS NOT NULL AND voice_channel_name <> ''))[1]
       AS voice_channel_name
FROM meetings
WHERE guild_id = $1
  AND voice_channel_id = ANY($2::TEXT[])
GROUP BY voice_channel_id
ORDER BY MAX(started_at) DESC NULLS LAST, voice_channel_id ASC
"#;

pub const LIST_ACTIVE_TENANT_GUILDS_BY_GUILD_IDS_SQL: &str = r#"
SELECT tg.guild_id,
       tg.tenant_id
FROM tenant_discord_guilds tg
JOIN tenants t ON t.id = tg.tenant_id
WHERE tg.guild_id = ANY($1::TEXT[])
  AND tg.status = 'active'
  AND t.status = 'active'
ORDER BY lower(tg.guild_id), tg.guild_id
"#;

pub const RESOLVE_TENANT_BY_GUILD_SQL: &str = r#"
SELECT t.id AS tenant_id,
       t.status AS tenant_status,
       to_char(t.period_anchor AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS period_anchor,
       tg.guild_id,
       tg.source
FROM tenant_discord_guilds tg
JOIN tenants t ON t.id = tg.tenant_id
WHERE tg.guild_id = $1
  AND tg.status = 'active'
  AND t.status = 'active'
LIMIT 1
"#;

pub const BACKFILL_DEFAULT_TENANTS_FROM_EXISTING_GUILDS_SQL: &str = r#"
WITH existing_guilds AS (
    SELECT DISTINCT guild_id
    FROM meetings
    WHERE guild_id IS NOT NULL
    UNION
    SELECT DISTINCT guild_id
    FROM guild_settings
    WHERE guild_id IS NOT NULL
), inserted_tenants AS (
    INSERT INTO tenants (id, status, created_at, updated_at)
    SELECT guild_id, 'active', NOW(), NOW()
    FROM existing_guilds
    ON CONFLICT (id) DO NOTHING
    RETURNING id
), inserted_installations AS (
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
    ON CONFLICT (id) DO NOTHING
    RETURNING id
)
SELECT
    (SELECT count(*) FROM inserted_tenants) AS tenants_inserted,
    (SELECT count(*) FROM inserted_installations) AS installations_inserted
"#;

pub const LIST_DOMAIN_KNOWLEDGE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $1
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
)
SELECT id,
       tenant_id,
       guild_id,
       content_type,
       title,
       body,
       active,
       version,
       updated_actor_user_id,
       to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
       archived_actor_user_id,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM domain_knowledge_items
WHERE guild_id = $1
  AND tenant_id = (SELECT tenant_id FROM active_tenant)
  AND ($2::TEXT::BOOLEAN OR archived_at IS NULL)
  AND (NULLIF($3, '') IS NULL OR content_type = $3)
ORDER BY active DESC, updated_at DESC, id DESC
"#;

pub const GET_DOMAIN_KNOWLEDGE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $1
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
)
SELECT id,
       tenant_id,
       guild_id,
       content_type,
       title,
       body,
       active,
       version,
       updated_actor_user_id,
       to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
       archived_actor_user_id,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM domain_knowledge_items
WHERE guild_id = $1
  AND id = $2
  AND tenant_id = (SELECT tenant_id FROM active_tenant)
"#;

pub const INSERT_DOMAIN_KNOWLEDGE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $2
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
)
INSERT INTO domain_knowledge_items (
    id, tenant_id, guild_id, content_type, title, body, active,
    version, updated_actor_user_id, created_at, updated_at
)
SELECT
    $1, tenant_id, $2, $3, $4, $5,
    $6::TEXT::BOOLEAN, 1, NULLIF($7, ''), NOW(), NOW()
FROM active_tenant
RETURNING id,
          tenant_id,
          guild_id,
          content_type,
          title,
          body,
          active,
          version,
          updated_actor_user_id,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const UPDATE_DOMAIN_KNOWLEDGE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $2
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
), updated AS (
    UPDATE domain_knowledge_items
    SET content_type = $3,
        title = $4,
        body = $5,
        active = COALESCE(NULLIF($6, '')::TEXT::BOOLEAN, active),
        version = CASE
            WHEN content_type IS DISTINCT FROM $3
              OR title IS DISTINCT FROM $4
              OR body IS DISTINCT FROM $5
              OR active IS DISTINCT FROM COALESCE(NULLIF($6, '')::TEXT::BOOLEAN, active)
            THEN version + 1
            ELSE version
        END,
        updated_actor_user_id = CASE
            WHEN content_type IS DISTINCT FROM $3
              OR title IS DISTINCT FROM $4
              OR body IS DISTINCT FROM $5
              OR active IS DISTINCT FROM COALESCE(NULLIF($6, '')::TEXT::BOOLEAN, active)
            THEN NULLIF($7, '')
            ELSE updated_actor_user_id
        END,
        updated_at = CASE
            WHEN content_type IS DISTINCT FROM $3
              OR title IS DISTINCT FROM $4
              OR body IS DISTINCT FROM $5
              OR active IS DISTINCT FROM COALESCE(NULLIF($6, '')::TEXT::BOOLEAN, active)
            THEN NOW()
            ELSE updated_at
        END
    WHERE id = $1
      AND guild_id = $2
      AND archived_at IS NULL
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
    RETURNING id,
              tenant_id,
              guild_id,
              content_type,
              title,
              body,
              active,
              version,
              updated_actor_user_id,
              to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
              archived_actor_user_id,
              to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
              to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
)
SELECT * FROM updated
"#;

pub const ACTIVATE_DOMAIN_KNOWLEDGE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $2
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
), updated AS (
    UPDATE domain_knowledge_items
    SET active = TRUE,
        archived_at = NULL,
        archived_actor_user_id = NULL,
        version = CASE
            WHEN NOT active OR archived_at IS NOT NULL THEN version + 1
            ELSE version
        END,
        updated_actor_user_id = CASE
            WHEN NOT active OR archived_at IS NOT NULL THEN NULLIF($3, '')
            ELSE updated_actor_user_id
        END,
        updated_at = CASE
            WHEN NOT active OR archived_at IS NOT NULL THEN NOW()
            ELSE updated_at
        END
    WHERE id = $1
      AND guild_id = $2
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
    RETURNING id,
              tenant_id,
              guild_id,
              content_type,
              title,
              body,
              active,
              version,
              updated_actor_user_id,
              to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
              archived_actor_user_id,
              to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
              to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
)
SELECT * FROM updated
"#;

pub const ARCHIVE_DOMAIN_KNOWLEDGE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $2
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
), updated AS (
    UPDATE domain_knowledge_items
    SET active = CASE
            WHEN archived_at IS NULL THEN FALSE
            ELSE active
        END,
        archived_at = COALESCE(archived_at, NOW()),
        archived_actor_user_id = COALESCE(archived_actor_user_id, NULLIF($3, '')),
        version = CASE
            WHEN archived_at IS NULL THEN version + 1
            ELSE version
        END,
        updated_actor_user_id = CASE
            WHEN archived_at IS NULL THEN NULLIF($3, '')
            ELSE updated_actor_user_id
        END,
        updated_at = CASE
            WHEN archived_at IS NULL THEN NOW()
            ELSE updated_at
        END
    WHERE id = $1
      AND guild_id = $2
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
    RETURNING id,
              tenant_id,
              guild_id,
              content_type,
              title,
              body,
              active,
              version,
              updated_actor_user_id,
              to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
              archived_actor_user_id,
              to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
              to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
)
SELECT * FROM updated
"#;

pub const RESOLVE_SINGLE_ACTIVE_TENANT_GUILD_SQL: &str = r#"
WITH active_installations AS (
    SELECT tg.id, tg.tenant_id, tg.guild_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $1
      AND tg.status = 'active'
      AND t.status = 'active'
)
SELECT id, tenant_id, guild_id
FROM active_installations
WHERE (SELECT COUNT(*) FROM active_installations) = 1
"#;

pub const AI_MEMORY_NOTE_COLUMNS_SQL: &str = r#"
id,
tenant_discord_guild_id,
tenant_id,
guild_id,
title,
body,
tags,
source_type,
source_meeting_id,
source_feedback_id,
to_char(confidence, 'FM0.000') AS confidence,
active,
pinned,
created_actor_user_id,
updated_actor_user_id,
to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_used_at,
to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
archived_actor_user_id
"#;

pub const LIST_AI_MEMORY_NOTES_SQL: &str = r#"
SELECT id,
       tenant_discord_guild_id,
       tenant_id,
       guild_id,
       title,
       body,
       array_to_string(tags, ',') AS tags,
       source_type,
       source_meeting_id,
       source_feedback_id,
       to_char(confidence, 'FM0.000') AS confidence,
       active,
       pinned,
       created_actor_user_id,
       updated_actor_user_id,
       to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_used_at,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
       to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
       archived_actor_user_id
FROM ai_memory_notes
WHERE tenant_id = $1
  AND guild_id = $2
  AND ($3::TEXT::BOOLEAN OR archived_at IS NULL)
  AND (NULLIF($4, '') IS NULL OR source_type = $4)
ORDER BY pinned DESC, updated_at DESC, id DESC
"#;

pub const GET_AI_MEMORY_NOTE_SQL: &str = r#"
SELECT id,
       tenant_discord_guild_id,
       tenant_id,
       guild_id,
       title,
       body,
       array_to_string(tags, ',') AS tags,
       source_type,
       source_meeting_id,
       source_feedback_id,
       to_char(confidence, 'FM0.000') AS confidence,
       active,
       pinned,
       created_actor_user_id,
       updated_actor_user_id,
       to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_used_at,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
       to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
       archived_actor_user_id
FROM ai_memory_notes
WHERE id = $1
  AND tenant_id = $2
  AND guild_id = $3
"#;

pub const INSERT_AI_MEMORY_NOTE_SQL: &str = r#"
INSERT INTO ai_memory_notes (
    id, tenant_discord_guild_id, tenant_id, guild_id, title, body, tags,
    source_type, source_meeting_id, source_feedback_id, confidence, active,
    pinned, created_actor_user_id, updated_actor_user_id, created_at, updated_at
)
VALUES (
    $1, $2, $3, $4, $5, $6, $7::TEXT[], $8, NULLIF($9, ''),
    NULLIF($10, ''), NULLIF($11, '')::TEXT::NUMERIC, $12::TEXT::BOOLEAN,
    $13::TEXT::BOOLEAN, $14, $14, NOW(), NOW()
)
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          title,
          body,
          array_to_string(tags, ',') AS tags,
          source_type,
          source_meeting_id,
          source_feedback_id,
          to_char(confidence, 'FM0.000') AS confidence,
          active,
          pinned,
          created_actor_user_id,
          updated_actor_user_id,
          to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_used_at,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id
"#;

pub const UPDATE_AI_MEMORY_NOTE_SQL: &str = r#"
UPDATE ai_memory_notes
SET title = $4,
    body = $5,
    tags = $6::TEXT[],
    confidence = NULLIF($7, '')::TEXT::NUMERIC,
    active = $8::TEXT::BOOLEAN,
    pinned = $9::TEXT::BOOLEAN,
    updated_actor_user_id = $10,
    updated_at = NOW()
WHERE id = $1
  AND tenant_id = $2
  AND guild_id = $3
  AND archived_at IS NULL
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          title,
          body,
          array_to_string(tags, ',') AS tags,
          source_type,
          source_meeting_id,
          source_feedback_id,
          to_char(confidence, 'FM0.000') AS confidence,
          active,
          pinned,
          created_actor_user_id,
          updated_actor_user_id,
          to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_used_at,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id
"#;

pub const SET_AI_MEMORY_PINNED_SQL: &str = r#"
UPDATE ai_memory_notes
SET pinned = $4::TEXT::BOOLEAN,
    updated_actor_user_id = $5,
    updated_at = NOW()
WHERE id = $1
  AND tenant_id = $2
  AND guild_id = $3
  AND archived_at IS NULL
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          title,
          body,
          array_to_string(tags, ',') AS tags,
          source_type,
          source_meeting_id,
          source_feedback_id,
          to_char(confidence, 'FM0.000') AS confidence,
          active,
          pinned,
          created_actor_user_id,
          updated_actor_user_id,
          to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_used_at,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id
"#;

pub const ARCHIVE_AI_MEMORY_NOTE_SQL: &str = r#"
UPDATE ai_memory_notes
SET active = FALSE,
    archived_at = COALESCE(archived_at, NOW()),
    archived_actor_user_id = COALESCE(archived_actor_user_id, $4),
    updated_actor_user_id = CASE WHEN archived_at IS NULL THEN $4 ELSE updated_actor_user_id END,
    updated_at = CASE WHEN archived_at IS NULL THEN NOW() ELSE updated_at END
WHERE id = $1
  AND tenant_id = $2
  AND guild_id = $3
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          title,
          body,
          array_to_string(tags, ',') AS tags,
          source_type,
          source_meeting_id,
          source_feedback_id,
          to_char(confidence, 'FM0.000') AS confidence,
          active,
          pinned,
          created_actor_user_id,
          updated_actor_user_id,
          to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_used_at,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id
"#;

pub const LIST_TRANSCRIPT_FEEDBACK_SQL: &str = r#"
SELECT id,
       tenant_discord_guild_id,
       tenant_id,
       guild_id,
       meeting_id,
       transcript_segment_id,
       feedback_type,
       term_type,
       original_text,
       corrected_text,
       speaker_id,
       corrected_speaker_id,
       note,
       target_domain_knowledge_id,
       target_ai_memory_note_id,
       actor_user_id,
       status,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
       reviewed_actor_user_id
FROM transcript_feedback
WHERE tenant_id = $1
  AND guild_id = $2
  AND (NULLIF($3, '') IS NULL OR status = $3)
  AND (NULLIF($4, '') IS NULL OR feedback_type = $4)
ORDER BY created_at DESC, id DESC
"#;

pub const INSERT_TRANSCRIPT_FEEDBACK_SQL: &str = r#"
INSERT INTO transcript_feedback (
    id, tenant_discord_guild_id, tenant_id, guild_id, meeting_id,
    transcript_segment_id, feedback_type, term_type, original_text, corrected_text,
    speaker_id, corrected_speaker_id, note, target_domain_knowledge_id,
    target_ai_memory_note_id, actor_user_id, status, created_at
)
VALUES (
    $1, $2, $3, $4, NULLIF($5, ''), NULLIF($6, ''), $7, NULLIF($8, ''),
    NULLIF($9, ''), NULLIF($10, ''), NULLIF($11, ''), NULLIF($12, ''),
    NULLIF($13, ''), NULLIF($14, ''), NULLIF($15, ''), $16, 'open', NOW()
)
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          meeting_id,
          transcript_segment_id,
          feedback_type,
          term_type,
          original_text,
          corrected_text,
          speaker_id,
          corrected_speaker_id,
          note,
          target_domain_knowledge_id,
          target_ai_memory_note_id,
          actor_user_id,
          status,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
          reviewed_actor_user_id
"#;

pub const INSERT_MEETING_TRANSCRIPT_FEEDBACK_SQL: &str = r#"
INSERT INTO transcript_feedback (
    id, tenant_discord_guild_id, tenant_id, guild_id, meeting_id,
    transcript_segment_id, feedback_type, term_type, original_text, corrected_text,
    speaker_id, corrected_speaker_id, note, target_domain_knowledge_id,
    target_ai_memory_note_id, actor_user_id, idempotency_key, status, created_at
)
VALUES (
    $1, $2, $3, $4, NULLIF($5, ''), NULLIF($6, ''), $7, NULLIF($8, ''),
    NULLIF($9, ''), NULLIF($10, ''), NULLIF($11, ''), NULLIF($12, ''),
    NULLIF($13, ''), NULLIF($14, ''), NULLIF($15, ''), $16, $17, 'open', NOW()
)
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          meeting_id,
          transcript_segment_id,
          feedback_type,
          term_type,
          original_text,
          corrected_text,
          speaker_id,
          corrected_speaker_id,
          note,
          target_domain_knowledge_id,
          target_ai_memory_note_id,
          actor_user_id,
          status,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
          reviewed_actor_user_id
"#;

pub const UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL: &str = r#"
UPDATE transcript_feedback
SET status = $4,
    target_domain_knowledge_id = CASE
        WHEN $4 = 'converted_to_domain_knowledge' THEN NULLIF($5, '')
        WHEN $4 = 'converted_to_ai_memory' THEN NULL
        ELSE target_domain_knowledge_id
    END,
    target_ai_memory_note_id = CASE
        WHEN $4 = 'converted_to_ai_memory' THEN NULLIF($6, '')
        WHEN $4 = 'converted_to_domain_knowledge' THEN NULL
        ELSE target_ai_memory_note_id
    END,
    reviewed_at = NOW(),
    reviewed_actor_user_id = $7
WHERE id = $1
  AND tenant_id = $2
  AND guild_id = $3
  AND status = 'open'
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          meeting_id,
          transcript_segment_id,
          feedback_type,
          term_type,
          original_text,
          corrected_text,
          speaker_id,
          corrected_speaker_id,
          note,
          target_domain_knowledge_id,
          target_ai_memory_note_id,
          actor_user_id,
          status,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
          reviewed_actor_user_id
"#;

pub const LIST_PERSON_ALIASES_SQL: &str = r#"
SELECT id,
       tenant_discord_guild_id,
       tenant_id,
       guild_id,
       canonical_name,
       alias,
       discord_user_id,
       source_type,
       source_meeting_id,
       source_feedback_id,
       to_char(confidence, 'FM0.000') AS confidence,
       active,
       review_status,
       created_actor_user_id,
       updated_actor_user_id,
       to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
       reviewed_actor_user_id,
       to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
       archived_actor_user_id,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM person_aliases
WHERE tenant_id = $1
  AND guild_id = $2
  AND ($3::TEXT::BOOLEAN OR archived_at IS NULL)
  AND (NULLIF($4, '') IS NULL OR review_status = $4)
ORDER BY active DESC, updated_at DESC, id DESC
"#;

pub const INSERT_PERSON_ALIAS_SQL: &str = r#"
INSERT INTO person_aliases (
    id, tenant_discord_guild_id, tenant_id, guild_id, canonical_name, alias,
    discord_user_id, source_type, source_meeting_id, source_feedback_id, confidence,
    active, review_status, created_actor_user_id, updated_actor_user_id, reviewed_at,
    reviewed_actor_user_id, created_at, updated_at
)
VALUES (
    $1, $2, $3, $4, $5, $6, NULLIF($7, ''), $8, NULLIF($9, ''),
    NULLIF($10, ''), NULLIF($11, '')::TEXT::NUMERIC, $12::TEXT::BOOLEAN,
    $13, $14, $14,
    CASE WHEN $13 = 'unreviewed' THEN NULL ELSE NOW() END,
    CASE WHEN $13 = 'unreviewed' THEN NULL ELSE $14 END,
    NOW(), NOW()
)
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          canonical_name,
          alias,
          discord_user_id,
          source_type,
          source_meeting_id,
          source_feedback_id,
          to_char(confidence, 'FM0.000') AS confidence,
          active,
          review_status,
          created_actor_user_id,
          updated_actor_user_id,
          to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
          reviewed_actor_user_id,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const UPSERT_VC_PARTICIPANT_PERSON_ALIAS_CANDIDATE_SQL: &str = r#"
INSERT INTO person_aliases (
    id, tenant_discord_guild_id, tenant_id, guild_id, canonical_name, alias,
    discord_user_id, source_type, source_meeting_id, source_feedback_id, confidence,
    active, review_status, created_actor_user_id, updated_actor_user_id, reviewed_at,
    reviewed_actor_user_id, created_at, updated_at
)
VALUES (
    $1, $2, $3, $4, $5, $6, NULLIF($7, ''), $8, NULLIF($9, ''),
    NULLIF($10, ''), NULLIF($11, '')::TEXT::NUMERIC, $12::TEXT::BOOLEAN,
    $13, $14, $14, NULL, NULL, NOW(), NOW()
)
ON CONFLICT (tenant_id, guild_id, lower(canonical_name), lower(alias)) WHERE active
DO UPDATE SET
    discord_user_id = COALESCE(person_aliases.discord_user_id, EXCLUDED.discord_user_id),
    source_meeting_id = EXCLUDED.source_meeting_id,
    confidence = EXCLUDED.confidence,
    updated_actor_user_id = EXCLUDED.updated_actor_user_id,
    updated_at = NOW()
WHERE person_aliases.source_type = 'vc_participant'
  AND person_aliases.review_status = 'unreviewed'
  AND person_aliases.archived_at IS NULL
  AND (
      person_aliases.confidence IS NULL
      OR person_aliases.confidence <= EXCLUDED.confidence
  )
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          canonical_name,
          alias,
          discord_user_id,
          source_type,
          source_meeting_id,
          source_feedback_id,
          to_char(confidence, 'FM0.000') AS confidence,
          active,
          review_status,
          created_actor_user_id,
          updated_actor_user_id,
          to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
          reviewed_actor_user_id,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const UPDATE_PERSON_ALIAS_SQL: &str = r#"
UPDATE person_aliases
SET canonical_name = $4,
    alias = $5,
    discord_user_id = NULLIF($6, ''),
    confidence = NULLIF($7, '')::TEXT::NUMERIC,
    active = $8::TEXT::BOOLEAN,
    review_status = $9,
    updated_actor_user_id = $10,
    reviewed_at = CASE
        WHEN $9 = 'unreviewed' THEN NULL
        WHEN review_status IS DISTINCT FROM $9 OR reviewed_at IS NULL THEN NOW()
        ELSE reviewed_at
    END,
    reviewed_actor_user_id = CASE
        WHEN $9 = 'unreviewed' THEN NULL
        WHEN review_status IS DISTINCT FROM $9 OR reviewed_actor_user_id IS NULL THEN $10
        ELSE reviewed_actor_user_id
    END,
    updated_at = NOW()
WHERE id = $1
  AND tenant_id = $2
  AND guild_id = $3
  AND archived_at IS NULL
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          canonical_name,
          alias,
          discord_user_id,
          source_type,
          source_meeting_id,
          source_feedback_id,
          to_char(confidence, 'FM0.000') AS confidence,
          active,
          review_status,
          created_actor_user_id,
          updated_actor_user_id,
          to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
          reviewed_actor_user_id,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const ARCHIVE_PERSON_ALIAS_SQL: &str = r#"
UPDATE person_aliases
SET active = FALSE,
    archived_at = COALESCE(archived_at, NOW()),
    archived_actor_user_id = COALESCE(archived_actor_user_id, $4),
    updated_actor_user_id = CASE WHEN archived_at IS NULL THEN $4 ELSE updated_actor_user_id END,
    updated_at = CASE WHEN archived_at IS NULL THEN NOW() ELSE updated_at END
WHERE id = $1
  AND tenant_id = $2
  AND guild_id = $3
RETURNING id,
          tenant_discord_guild_id,
          tenant_id,
          guild_id,
          canonical_name,
          alias,
          discord_user_id,
          source_type,
          source_meeting_id,
          source_feedback_id,
          to_char(confidence, 'FM0.000') AS confidence,
          active,
          review_status,
          created_actor_user_id,
          updated_actor_user_id,
          to_char(reviewed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS reviewed_at,
          reviewed_actor_user_id,
          to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
          archived_actor_user_id,
          to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
          to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const LIST_SUMMARY_TEMPLATES_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $1
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
)
SELECT id,
       tenant_id,
       guild_id,
       name,
       template,
       active,
       version,
       updated_actor_user_id,
       to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
       archived_actor_user_id,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM summary_templates
WHERE guild_id = $1
  AND tenant_id = (SELECT tenant_id FROM active_tenant)
  AND ($2::TEXT::BOOLEAN OR archived_at IS NULL)
ORDER BY active DESC, updated_at DESC, id DESC
"#;

pub const GET_SUMMARY_TEMPLATE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $1
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
)
SELECT id,
       tenant_id,
       guild_id,
       name,
       template,
       active,
       version,
       updated_actor_user_id,
       to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
       archived_actor_user_id,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM summary_templates
WHERE guild_id = $1
  AND id = $2
  AND tenant_id = (SELECT tenant_id FROM active_tenant)
"#;

pub const GET_ACTIVE_SUMMARY_TEMPLATE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $1
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
)
SELECT id,
       tenant_id,
       guild_id,
       name,
       template,
       active,
       version,
       updated_actor_user_id,
       to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
       archived_actor_user_id,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
       to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
FROM summary_templates
WHERE guild_id = $1
  AND tenant_id = (SELECT tenant_id FROM active_tenant)
  AND active = TRUE
  AND archived_at IS NULL
ORDER BY updated_at DESC, id DESC
LIMIT 1
"#;

pub const INSERT_SUMMARY_TEMPLATE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $2
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
), deactivate_others AS (
    UPDATE summary_templates
    SET active = FALSE,
        version = version + 1,
        updated_actor_user_id = NULLIF($6, ''),
        updated_at = NOW()
    WHERE $5::TEXT::BOOLEAN
      AND guild_id = $2
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
      AND active = TRUE
      AND archived_at IS NULL
), inserted AS (
    INSERT INTO summary_templates (
        id, tenant_id, guild_id, name, template, active,
        version, updated_actor_user_id, created_at, updated_at
    )
    SELECT
        $1, tenant_id, $2, $3, $4,
        $5::TEXT::BOOLEAN, 1, NULLIF($6, ''), NOW(), NOW()
    FROM active_tenant
    RETURNING id,
              tenant_id,
              guild_id,
              name,
              template,
              active,
              version,
              updated_actor_user_id,
              to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
              archived_actor_user_id,
              to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
              to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
)
SELECT * FROM inserted
"#;

pub const UPDATE_SUMMARY_TEMPLATE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $2
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
), desired AS (
    SELECT COALESCE(NULLIF($5, '')::TEXT::BOOLEAN, active) AS active
    FROM summary_templates
    WHERE id = $1
      AND guild_id = $2
      AND archived_at IS NULL
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
), deactivate_others AS (
    UPDATE summary_templates
    SET active = FALSE,
        version = version + 1,
        updated_actor_user_id = NULLIF($6, ''),
        updated_at = NOW()
    WHERE (SELECT active FROM desired)
      AND id <> $1
      AND guild_id = $2
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
      AND active = TRUE
      AND archived_at IS NULL
), updated AS (
    UPDATE summary_templates
    SET name = $3,
        template = $4,
        active = (SELECT active FROM desired),
        version = CASE
            WHEN name IS DISTINCT FROM $3
              OR template IS DISTINCT FROM $4
              OR active IS DISTINCT FROM (SELECT active FROM desired)
            THEN version + 1
            ELSE version
        END,
        updated_actor_user_id = CASE
            WHEN name IS DISTINCT FROM $3
              OR template IS DISTINCT FROM $4
              OR active IS DISTINCT FROM (SELECT active FROM desired)
            THEN NULLIF($6, '')
            ELSE updated_actor_user_id
        END,
        updated_at = CASE
            WHEN name IS DISTINCT FROM $3
              OR template IS DISTINCT FROM $4
              OR active IS DISTINCT FROM (SELECT active FROM desired)
            THEN NOW()
            ELSE updated_at
        END
    WHERE id = $1
      AND guild_id = $2
      AND archived_at IS NULL
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
    RETURNING id,
              tenant_id,
              guild_id,
              name,
              template,
              active,
              version,
              updated_actor_user_id,
              to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
              archived_actor_user_id,
              to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
              to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
)
SELECT * FROM updated
"#;

pub const ACTIVATE_SUMMARY_TEMPLATE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $2
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
), deactivate_others AS (
    UPDATE summary_templates
    SET active = FALSE,
        version = version + 1,
        updated_actor_user_id = NULLIF($3, ''),
        updated_at = NOW()
    WHERE id <> $1
      AND guild_id = $2
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
      AND active = TRUE
      AND archived_at IS NULL
), updated AS (
    UPDATE summary_templates
    SET active = TRUE,
        archived_at = NULL,
        archived_actor_user_id = NULL,
        version = CASE
            WHEN NOT active OR archived_at IS NOT NULL THEN version + 1
            ELSE version
        END,
        updated_actor_user_id = CASE
            WHEN NOT active OR archived_at IS NOT NULL THEN NULLIF($3, '')
            ELSE updated_actor_user_id
        END,
        updated_at = CASE
            WHEN NOT active OR archived_at IS NOT NULL THEN NOW()
            ELSE updated_at
        END
    WHERE id = $1
      AND guild_id = $2
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
    RETURNING id,
              tenant_id,
              guild_id,
              name,
              template,
              active,
              version,
              updated_actor_user_id,
              to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
              archived_actor_user_id,
              to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
              to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
)
SELECT * FROM updated
"#;

pub const ARCHIVE_SUMMARY_TEMPLATE_SQL: &str = r#"
WITH active_tenant AS (
    SELECT tg.tenant_id
    FROM tenant_discord_guilds tg
    JOIN tenants t ON t.id = tg.tenant_id
    WHERE tg.guild_id = $2
      AND tg.status = 'active'
      AND t.status = 'active'
    ORDER BY tg.effective_at DESC
    LIMIT 1
), updated AS (
    UPDATE summary_templates
    SET active = CASE
            WHEN archived_at IS NULL THEN FALSE
            ELSE active
        END,
        archived_at = COALESCE(archived_at, NOW()),
        archived_actor_user_id = COALESCE(archived_actor_user_id, NULLIF($3, '')),
        version = CASE
            WHEN archived_at IS NULL THEN version + 1
            ELSE version
        END,
        updated_actor_user_id = CASE
            WHEN archived_at IS NULL THEN NULLIF($3, '')
            ELSE updated_actor_user_id
        END,
        updated_at = CASE
            WHEN archived_at IS NULL THEN NOW()
            ELSE updated_at
        END
    WHERE id = $1
      AND guild_id = $2
      AND tenant_id = (SELECT tenant_id FROM active_tenant)
    RETURNING id,
              tenant_id,
              guild_id,
              name,
              template,
              active,
              version,
              updated_actor_user_id,
              to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS archived_at,
              archived_actor_user_id,
              to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
              to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
)
SELECT * FROM updated
"#;
