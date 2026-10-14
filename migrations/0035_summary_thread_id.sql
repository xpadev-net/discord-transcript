ALTER TABLE meetings
    ADD COLUMN IF NOT EXISTS summary_thread_id TEXT;
