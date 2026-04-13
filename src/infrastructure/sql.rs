pub const INITIAL_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meetings (
    id TEXT PRIMARY KEY,
    guild_id TEXT NOT NULL,
    voice_channel_id TEXT NOT NULL,
    report_channel_id TEXT NOT NULL,
    status_message_channel_id TEXT,
    status_message_id TEXT,
    started_by_user_id TEXT NOT NULL,
    title TEXT,
    status TEXT NOT NULL,
    stop_reason TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    stopped_at TIMESTAMPTZ,
    meeting_duration_seconds INTEGER,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_meetings_guild_status
    ON meetings (guild_id, status);

CREATE TABLE IF NOT EXISTS transcripts (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    speaker_id TEXT NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    text TEXT NOT NULL,
    confidence DOUBLE PRECISION,
    is_noisy BOOLEAN NOT NULL DEFAULT FALSE,
    source TEXT NOT NULL DEFAULT 'voice',
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_transcripts_meeting
    ON transcripts (meeting_id, start_ms);

CREATE TABLE IF NOT EXISTS meeting_speakers (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    speaker_id TEXT NOT NULL,
    username TEXT,
    nickname TEXT,
    display_name TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (meeting_id, speaker_id)
);

CREATE TABLE IF NOT EXISTS summaries (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    markdown TEXT NOT NULL,
    raw_json JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (meeting_id, version)
);

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    job_type TEXT NOT NULL,
    status TEXT NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_jobs_meeting_type_status
    ON jobs (meeting_id, job_type, status);

CREATE INDEX IF NOT EXISTS idx_jobs_claim
    ON jobs (job_type, status, created_at);

CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    storage_url TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_artifacts_meeting_kind
    ON artifacts (meeting_id, kind);
"#;

/// Incremental migrations applied after the initial schema.
/// Each statement must be idempotent (IF NOT EXISTS / IF EXISTS).
pub const INCREMENTAL_MIGRATIONS_SQL: &str = r#"
ALTER TABLE transcripts ADD COLUMN IF NOT EXISTS is_noisy BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE transcripts ADD COLUMN IF NOT EXISTS source TEXT NOT NULL DEFAULT 'voice';
ALTER TABLE meetings ADD COLUMN IF NOT EXISTS status_message_channel_id TEXT;
ALTER TABLE meetings ADD COLUMN IF NOT EXISTS status_message_id TEXT;
CREATE TABLE IF NOT EXISTS meeting_speakers (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    speaker_id TEXT NOT NULL,
    username TEXT,
    nickname TEXT,
    display_name TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (meeting_id, speaker_id)
);
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

pub const ENQUEUE_JOB_SQL: &str = r#"
INSERT INTO jobs (id, meeting_id, job_type, status, retry_count, created_at, updated_at)
VALUES ($1, $2, $3, 'queued', 0, NOW(), NOW())
"#;

pub const CLAIM_JOB_SQL: &str = r#"
UPDATE jobs
SET status = 'running',
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
pub fn build_insert_transcripts_sql(count: usize) -> String {
    let mut sql = String::from(
        "INSERT INTO transcripts (id, meeting_id, speaker_id, start_ms, end_ms, text, confidence, is_noisy, source) VALUES ",
    );
    for i in 0..count {
        let base = i * 9;
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
    sql.push_str(" ON CONFLICT (id) DO NOTHING");
    sql
}
