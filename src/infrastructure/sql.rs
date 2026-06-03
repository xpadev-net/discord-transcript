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
  AND observed_at >= NOW() - make_interval(secs => $3::BIGINT)
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
    leased_until=NULL,
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
  AND status = 'running'
"#;

pub const RECOVERY_SUMMARY_JOB_STATUS_SQL: &str = r#"
SELECT status
FROM jobs
WHERE id=$1
  AND job_type='summarize'
LIMIT 1
"#;

pub const ENQUEUE_JOB_SQL: &str = r#"
INSERT INTO jobs (id, meeting_id, job_type, status, retry_count, created_at, updated_at)
VALUES ($1, $2, $3, 'queued', 0, NOW(), NOW())
"#;

pub const CLAIM_JOB_SQL: &str = r#"
UPDATE jobs
SET status = 'running',
    leased_until = NOW() + INTERVAL '90 seconds',
    updated_at = NOW()
WHERE id = (
    SELECT id
    FROM jobs
    WHERE job_type = $1
      AND status = 'queued'
    ORDER BY created_at ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
RETURNING id, meeting_id, job_type, status, retry_count, error_message
"#;

pub const CLAIM_JOB_BY_ID_SQL: &str = r#"
UPDATE jobs
SET status = 'running',
    leased_until = NOW() + INTERVAL '90 seconds',
    updated_at = NOW()
WHERE id = $1
  AND status = 'queued'
RETURNING id, meeting_id, job_type, status, retry_count, error_message
"#;

pub const MARK_JOB_DONE_SQL: &str = r#"
UPDATE jobs
SET status = 'done',
    error_message = NULL,
    updated_at = NOW()
WHERE id = $1
  AND status = 'running'
"#;

pub const MARK_JOB_FAILED_SQL: &str = r#"
UPDATE jobs
SET status = 'failed',
    error_message = $2,
    updated_at = NOW()
WHERE id = $1
  AND status = 'running'
"#;

pub const RETRY_JOB_SQL: &str = r#"
UPDATE jobs
SET
  status = CASE WHEN retry_count + 1 > $3::integer THEN 'failed' ELSE 'queued' END,
  retry_count = retry_count + 1,
  error_message = $2,
  updated_at = NOW()
WHERE id = $1
  AND status = 'running'
RETURNING status
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
/// reserve leading parameters for wrapping CTEs.
pub fn build_insert_transcripts_sql_with_offset(count: usize, param_offset: usize) -> String {
    let mut sql = String::from(
        "INSERT INTO transcripts (id, meeting_id, speaker_id, start_ms, end_ms, text, confidence, is_noisy, source) VALUES ",
    );
    for i in 0..count {
        let base = i * 9 + param_offset;
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!(
            "(${}, ${}, ${}, ${}::TEXT::INTEGER, ${}::TEXT::INTEGER, ${}, NULLIF(${},'')::TEXT::DOUBLE PRECISION, ${}::TEXT::BOOLEAN, ${})",
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
        source = EXCLUDED.source",
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
)
INSERT INTO audit_events (
    id, tenant_id, guild_id, actor_user_id, action, resource_type, resource_id,
    request_metadata, detail_json, occurred_at, created_at
) VALUES (
    $1,
    COALESCE(NULLIF($2, ''), (SELECT tenant_id FROM active_tenant)),
    NULLIF($3, ''),
    NULLIF($4, ''),
    $5,
    $6,
    NULLIF($7, ''),
    COALESCE(NULLIF($8, '')::jsonb, '{}'::jsonb),
    COALESCE(NULLIF($9, '')::jsonb, '{}'::jsonb),
    COALESCE(NULLIF($10, '')::timestamptz, NOW()),
    NOW()
)
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
        id, guild_id, voice_channel_id, report_channel_id,
        status_message_channel_id, status_message_id, started_by_user_id, status
    )
    VALUES($1,$2,$3,$4,NULLIF($5,''),NULLIF($6,''),$7,'recording')
    RETURNING id
)
INSERT INTO meeting_effective_settings (
    meeting_id, whisper_language, whisper_vad, whisper_beam_size,
    whisper_suppress_non_speech, whisper_prompt, whisper_temperature,
    whisper_resample_to_16k, auto_stop_grace_seconds, retention_raw_audio_ttl_days,
    retention_transcript_ttl_days, retention_summary_ttl_days, summary_enabled,
    summary_template_id, domain_knowledge_version_id, updated_at
)
SELECT id, NULLIF($8,''), $9::TEXT::BOOLEAN, $10::TEXT::INTEGER,
       $11::TEXT::BOOLEAN, NULLIF($12,''), $13::TEXT::DOUBLE PRECISION,
       $14::TEXT::BOOLEAN, $15::TEXT::BIGINT, $16::TEXT::INTEGER,
       $17::TEXT::INTEGER, NULLIF($18,'')::TEXT::INTEGER,
       $19::TEXT::BOOLEAN, NULLIF($20,''), NULLIF($21,''), NOW()
FROM inserted_meeting
"#;

pub const INSERT_SCHEDULED_MEETING_WITH_EFFECTIVE_SETTINGS_SQL: &str = r#"
WITH inserted_meeting AS (
    INSERT INTO meetings(
        id, guild_id, voice_channel_id, report_channel_id,
        status_message_channel_id, status_message_id, started_by_user_id, status
    )
    VALUES($1,$2,$3,$4,NULLIF($5,''),NULLIF($6,''),$7,'scheduled')
    RETURNING id
)
INSERT INTO meeting_effective_settings (
    meeting_id, whisper_language, whisper_vad, whisper_beam_size,
    whisper_suppress_non_speech, whisper_prompt, whisper_temperature,
    whisper_resample_to_16k, auto_stop_grace_seconds, retention_raw_audio_ttl_days,
    retention_transcript_ttl_days, retention_summary_ttl_days, summary_enabled,
    summary_template_id, domain_knowledge_version_id, updated_at
)
SELECT id, NULLIF($8,''), $9::TEXT::BOOLEAN, $10::TEXT::INTEGER,
       $11::TEXT::BOOLEAN, NULLIF($12,''), $13::TEXT::DOUBLE PRECISION,
       $14::TEXT::BOOLEAN, $15::TEXT::BIGINT, $16::TEXT::INTEGER,
       $17::TEXT::INTEGER, NULLIF($18,'')::TEXT::INTEGER,
       $19::TEXT::BOOLEAN, NULLIF($20,''), NULLIF($21,''), NOW()
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
       stop_reason
FROM meetings
WHERE guild_id = $1
ORDER BY started_at DESC
LIMIT $2 OFFSET $3
"#;

pub const COUNT_GUILD_MEETINGS_SQL: &str = r#"
SELECT count(*) FROM meetings WHERE guild_id = $1
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
