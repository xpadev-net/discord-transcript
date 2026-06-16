WITH ranked_blocking_meetings AS (
    SELECT
        id,
        status,
        ROW_NUMBER() OVER (
            PARTITION BY guild_id
            ORDER BY
                CASE WHEN status = 'recording' THEN 0 ELSE 1 END,
                started_at DESC,
                created_at DESC,
                id DESC
        ) AS rn
    FROM meetings
    WHERE status IN ('scheduled', 'recording')
)
UPDATE meetings
SET
    status = CASE
        WHEN ranked_blocking_meetings.status = 'scheduled' THEN 'aborted'
        ELSE 'failed'
    END,
    error_message = COALESCE(
        error_message,
        'Superseded by active meeting uniqueness migration'
    ),
    stopped_at = COALESCE(stopped_at, NOW()),
    meeting_duration_seconds = CASE
        WHEN ranked_blocking_meetings.status = 'recording' THEN COALESCE(
            meeting_duration_seconds,
            GREATEST(0, EXTRACT(EPOCH FROM (NOW() - started_at))::INTEGER)
        )
        ELSE meeting_duration_seconds
    END,
    updated_at = NOW()
FROM ranked_blocking_meetings
WHERE meetings.id = ranked_blocking_meetings.id
  AND ranked_blocking_meetings.rn > 1;

CREATE UNIQUE INDEX IF NOT EXISTS idx_meetings_one_active_blocking_per_guild
    ON meetings (guild_id)
    WHERE status IN ('scheduled', 'recording');
