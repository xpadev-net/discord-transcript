ALTER TABLE live_transcription_chunks
    ADD COLUMN IF NOT EXISTS audio_path TEXT;

ALTER TABLE live_transcription_chunks
    DROP CONSTRAINT IF EXISTS live_transcription_chunks_status_check;

ALTER TABLE live_transcription_chunks
    ADD CONSTRAINT live_transcription_chunks_status_check
    CHECK (status IN ('queued', 'running', 'done', 'failed'));

-- A process may exit while a worker is running. Keep its persisted WAV in the
-- durable outbox so the next runtime can resume it.
UPDATE live_transcription_chunks
SET status = CASE WHEN audio_path IS NULL THEN 'failed' ELSE 'queued' END,
    error_message = CASE WHEN audio_path IS NULL THEN 'audio path unavailable during outbox migration' ELSE NULL END,
    updated_at = NOW()
WHERE status = 'running';
