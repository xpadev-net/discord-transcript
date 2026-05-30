CREATE TABLE IF NOT EXISTS guild_settings (
    guild_id TEXT PRIMARY KEY,
    whisper_language TEXT,
    whisper_language_explicit BOOLEAN NOT NULL DEFAULT FALSE,
    whisper_vad BOOLEAN,
    auto_stop_grace_seconds BIGINT,
    retention_raw_audio_ttl_days INTEGER,
    retention_transcript_ttl_days INTEGER,
    summary_enabled BOOLEAN,
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_guild_settings_updated ON guild_settings (updated_at DESC);

-- Composite index for guild meeting listing queries
CREATE INDEX IF NOT EXISTS idx_meetings_guild_started ON meetings (guild_id, started_at DESC);
