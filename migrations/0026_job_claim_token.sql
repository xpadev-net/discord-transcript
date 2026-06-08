ALTER TABLE jobs
ADD COLUMN IF NOT EXISTS claim_token TEXT;

CREATE INDEX IF NOT EXISTS idx_jobs_running_claim
    ON jobs (id, claim_token)
    WHERE status = 'running';
