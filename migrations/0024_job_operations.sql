ALTER TABLE jobs
ADD COLUMN IF NOT EXISTS next_run_at TIMESTAMPTZ;

ALTER TABLE jobs
ADD COLUMN IF NOT EXISTS finished_at TIMESTAMPTZ;

ALTER TABLE jobs
ADD COLUMN IF NOT EXISTS dead_lettered_at TIMESTAMPTZ;

ALTER TABLE jobs
ADD COLUMN IF NOT EXISTS canceled_at TIMESTAMPTZ;

ALTER TABLE jobs
ADD COLUMN IF NOT EXISTS cancel_reason TEXT;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'jobs'::regclass
          AND conname = 'jobs_status_check'
          AND pg_get_constraintdef(oid) NOT LIKE '%canceled%'
    ) THEN
        ALTER TABLE jobs DROP CONSTRAINT jobs_status_check;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'jobs'::regclass
          AND conname = 'jobs_status_check'
    ) THEN
        ALTER TABLE jobs
        ADD CONSTRAINT jobs_status_check
        CHECK (status IN ('queued', 'running', 'failed', 'done', 'canceled'));
    END IF;
END
$$;

CREATE INDEX IF NOT EXISTS idx_jobs_claim_due
    ON jobs (job_type, status, next_run_at, created_at)
    WHERE status = 'queued';

CREATE INDEX IF NOT EXISTS idx_jobs_dead_lettered
    ON jobs (dead_lettered_at DESC, updated_at DESC)
    WHERE status = 'failed';
