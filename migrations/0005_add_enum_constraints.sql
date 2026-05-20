DO $$
BEGIN
    ALTER TABLE meetings
    ADD CONSTRAINT meetings_status_check
    CHECK (status IN ('scheduled', 'recording', 'stopping', 'transcribing', 'summarizing', 'posted', 'failed', 'aborted'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE jobs
    ADD CONSTRAINT jobs_status_check
    CHECK (status IN ('queued', 'running', 'failed', 'done'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE jobs
    ADD CONSTRAINT jobs_job_type_check
    CHECK (job_type IN ('transcribe', 'summarize', 'cleanup'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;
