CREATE TABLE IF NOT EXISTS meeting_effective_settings (
    meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
    whisper_language TEXT,
    whisper_vad BOOLEAN NOT NULL,
    whisper_beam_size INTEGER NOT NULL,
    whisper_suppress_non_speech BOOLEAN NOT NULL,
    whisper_prompt TEXT,
    whisper_temperature DOUBLE PRECISION NOT NULL,
    whisper_resample_to_16k BOOLEAN NOT NULL,
    auto_stop_grace_seconds BIGINT NOT NULL,
    retention_raw_audio_ttl_days INTEGER NOT NULL,
    retention_transcript_ttl_days INTEGER NOT NULL,
    retention_summary_ttl_days INTEGER,
    summary_enabled BOOLEAN NOT NULL,
    summary_template_id TEXT,
    domain_knowledge_version_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    ALTER TABLE meeting_effective_settings
    ADD CONSTRAINT meeting_effective_settings_whisper_beam_size_check
    CHECK (whisper_beam_size > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE meeting_effective_settings
    ADD CONSTRAINT meeting_effective_settings_whisper_temperature_check
    CHECK (whisper_temperature >= 0.0 AND whisper_temperature <= 1.0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE meeting_effective_settings
    ADD CONSTRAINT meeting_effective_settings_auto_stop_grace_seconds_check
    CHECK (auto_stop_grace_seconds > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE meeting_effective_settings
    ADD CONSTRAINT meeting_effective_settings_retention_raw_audio_ttl_days_check
    CHECK (retention_raw_audio_ttl_days > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE meeting_effective_settings
    ADD CONSTRAINT meeting_effective_settings_retention_transcript_ttl_days_check
    CHECK (retention_transcript_ttl_days > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE meeting_effective_settings
    ADD CONSTRAINT meeting_effective_settings_retention_summary_ttl_days_check
    CHECK (retention_summary_ttl_days IS NULL OR retention_summary_ttl_days > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;
