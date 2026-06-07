ALTER TABLE transcript_feedback
    ADD COLUMN IF NOT EXISTS idempotency_key TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_transcript_feedback_meeting_actor_identity
    ON transcript_feedback (tenant_id, guild_id, meeting_id, actor_user_id, idempotency_key)
    WHERE meeting_id IS NOT NULL
      AND idempotency_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_transcript_feedback_meeting_actor_created_at
    ON transcript_feedback (tenant_id, guild_id, meeting_id, actor_user_id, created_at DESC)
    WHERE meeting_id IS NOT NULL;

CREATE OR REPLACE FUNCTION enforce_transcript_feedback_daily_quota()
RETURNS TRIGGER AS $$
DECLARE
    quota_limit CONSTANT INTEGER := 20;
    quota_day_start TIMESTAMPTZ;
    quota_day_end TIMESTAMPTZ;
BEGIN
    IF NEW.meeting_id IS NULL THEN
        RETURN NEW;
    END IF;

    quota_day_start := date_trunc('day', NEW.created_at AT TIME ZONE 'UTC') AT TIME ZONE 'UTC';
    quota_day_end := quota_day_start + INTERVAL '1 day';

    PERFORM pg_advisory_xact_lock(
        hashtextextended(
            concat_ws(
                '|',
                NEW.tenant_id,
                NEW.guild_id,
                NEW.meeting_id,
                NEW.actor_user_id,
                to_char(quota_day_start AT TIME ZONE 'UTC', 'YYYY-MM-DD')
            ),
            0
        )
    );

    IF NEW.idempotency_key IS NOT NULL
       AND EXISTS (
           SELECT 1
           FROM transcript_feedback existing
           WHERE existing.tenant_id = NEW.tenant_id
             AND existing.guild_id = NEW.guild_id
             AND existing.meeting_id = NEW.meeting_id
             AND existing.actor_user_id = NEW.actor_user_id
             AND existing.idempotency_key = NEW.idempotency_key
       ) THEN
        RAISE EXCEPTION 'meeting feedback duplicate submission'
            USING ERRCODE = '23505',
                  CONSTRAINT = 'idx_transcript_feedback_meeting_actor_identity';
    END IF;

    IF (
        SELECT COUNT(*)
        FROM transcript_feedback existing
        WHERE existing.tenant_id = NEW.tenant_id
          AND existing.guild_id = NEW.guild_id
          AND existing.meeting_id = NEW.meeting_id
          AND existing.actor_user_id = NEW.actor_user_id
          AND existing.created_at >= quota_day_start
          AND existing.created_at < quota_day_end
    ) >= quota_limit THEN
        RAISE EXCEPTION 'meeting feedback daily quota exceeded'
            USING ERRCODE = '23514',
                  CONSTRAINT = 'transcript_feedback_daily_quota_check';
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_enforce_transcript_feedback_daily_quota ON transcript_feedback;

CREATE TRIGGER trg_enforce_transcript_feedback_daily_quota
    BEFORE INSERT ON transcript_feedback
    FOR EACH ROW
    EXECUTE FUNCTION enforce_transcript_feedback_daily_quota();
