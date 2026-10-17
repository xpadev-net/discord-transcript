CREATE TABLE IF NOT EXISTS meeting_schedules (
    id TEXT PRIMARY KEY,
    guild_id TEXT NOT NULL,
    voice_channel_id TEXT NOT NULL,
    report_channel_id TEXT NOT NULL,
    created_by_user_id TEXT NOT NULL,
    timezone TEXT NOT NULL,
    local_start_time TIME NOT NULL,
    duration_seconds BIGINT NOT NULL CHECK (duration_seconds BETWEEN 60 AND 86400),
    next_start_at TIMESTAMPTZ NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS meeting_schedules_due_idx
    ON meeting_schedules(next_start_at) WHERE active = TRUE;
CREATE INDEX IF NOT EXISTS meeting_schedules_guild_idx
    ON meeting_schedules(guild_id, active, next_start_at);
