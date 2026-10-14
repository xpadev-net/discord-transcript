ALTER TABLE guild_settings
    ADD COLUMN IF NOT EXISTS summary_in_status_thread BOOLEAN;
