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

DO $$
BEGIN
    ALTER TABLE live_transcription_chunks
    ADD CONSTRAINT live_transcription_chunks_status_check
    CHECK (status IN ('running', 'done', 'failed'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS idx_live_transcription_chunks_meeting_status
    ON live_transcription_chunks (meeting_id, status, start_ms);
