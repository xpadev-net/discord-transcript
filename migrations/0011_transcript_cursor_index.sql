CREATE INDEX IF NOT EXISTS idx_transcripts_meeting_cursor
    ON transcripts (meeting_id, created_at, id)
    WHERE NOT is_deleted;
