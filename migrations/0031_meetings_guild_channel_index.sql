-- Every summary job looks up meetings by (guild_id, voice_channel_id) to
-- resolve the channel-scope allowlist; index that pair. Built CONCURRENTLY
-- so the migration does not block meeting writes during a rolling deploy;
-- runs outside the migration transaction (see migration_statements).
-- A previous failed concurrent build can leave an INVALID index behind that
-- IF NOT EXISTS would keep, so drop it first — but only when invalid:
-- invalid indexes never serve reads, so a plain DROP cannot stall queries,
-- and a valid index (created but unrecorded) is preserved.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM pg_class c
          JOIN pg_index i ON i.indexrelid = c.oid
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relname = 'idx_meetings_guild_channel'
           AND n.nspname = current_schema()
           AND NOT i.indisvalid
    ) THEN
        DROP INDEX idx_meetings_guild_channel;
    END IF;
END $$;
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_meetings_guild_channel
    ON meetings (guild_id, voice_channel_id);
