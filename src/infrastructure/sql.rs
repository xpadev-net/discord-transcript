pub const INITIAL_SCHEMA_SQL: &str = include_str!("../../migrations/0001_mvp_schema.sql");

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
  updated_at = NOW()
WHERE id = $2
  AND status = 'recording'
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
