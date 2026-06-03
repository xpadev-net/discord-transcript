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
    retention_raw_cleaned_at TIMESTAMPTZ,
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
    transcript_stage TEXT NOT NULL DEFAULT 'final',
    live_chunk_id TEXT,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_transcripts_meeting
    ON transcripts (meeting_id, start_ms);

ALTER TABLE transcripts
ADD COLUMN IF NOT EXISTS is_deleted BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS idx_transcripts_meeting_cursor
    ON transcripts (meeting_id, created_at, id)
    WHERE NOT is_deleted;

ALTER TABLE transcripts
ADD COLUMN IF NOT EXISTS transcript_stage TEXT NOT NULL DEFAULT 'final';

ALTER TABLE transcripts
ADD COLUMN IF NOT EXISTS live_chunk_id TEXT;

DO $$
BEGIN
    ALTER TABLE transcripts
    ADD CONSTRAINT transcripts_stage_check CHECK (transcript_stage IN ('live', 'final'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS idx_transcripts_meeting_stage
    ON transcripts (meeting_id, transcript_stage, start_ms);

CREATE INDEX IF NOT EXISTS idx_transcripts_live_chunk
    ON transcripts (live_chunk_id);

CREATE TABLE IF NOT EXISTS live_transcription_chunks (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    speaker_id TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    start_ms BIGINT NOT NULL,
    timeline_base_ms BIGINT,
    status TEXT NOT NULL,
    error_message TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (meeting_id, speaker_id, sequence, start_ms)
);

CREATE INDEX IF NOT EXISTS idx_live_transcription_chunks_meeting_status
    ON live_transcription_chunks (meeting_id, status, start_ms);

DO $$
BEGIN
    ALTER TABLE live_transcription_chunks
    ADD CONSTRAINT live_transcription_chunks_status_check
    CHECK (status IN ('running', 'done', 'failed'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

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
