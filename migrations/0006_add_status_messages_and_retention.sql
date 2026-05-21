ALTER TABLE meetings ADD COLUMN IF NOT EXISTS status_message_channel_id TEXT;
ALTER TABLE meetings ADD COLUMN IF NOT EXISTS status_message_id TEXT;
ALTER TABLE meetings ADD COLUMN IF NOT EXISTS retention_raw_cleaned_at TIMESTAMPTZ;
