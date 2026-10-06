-- Every summary job looks up meetings by (guild_id, voice_channel_id) to
-- resolve the channel-scope allowlist; index that pair.
CREATE INDEX IF NOT EXISTS idx_meetings_guild_channel
    ON meetings (guild_id, voice_channel_id);
