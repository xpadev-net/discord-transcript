-- Every summary job looks up meetings by (guild_id, voice_channel_id) to
-- resolve the channel-scope allowlist; index that pair. Built CONCURRENTLY
-- so the migration does not block meeting writes during a rolling deploy;
-- runs outside the migration transaction (see migration_statements).
-- A previous failed concurrent build can leave an invalid index behind, so
-- drop any leftover first to keep this migration idempotent.
DROP INDEX IF EXISTS idx_meetings_guild_channel;
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_meetings_guild_channel
    ON meetings (guild_id, voice_channel_id);
