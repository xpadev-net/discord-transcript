DO $$
BEGIN
    ALTER TABLE meetings
    ADD CONSTRAINT meetings_stop_reason_check
    CHECK (
        stop_reason IS NULL
        OR stop_reason IN ('manual', 'auto_empty', 'client_disconnect', 'error')
    ) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE meetings VALIDATE CONSTRAINT meetings_stop_reason_check;
EXCEPTION
    WHEN undefined_object THEN NULL;
END
$$;
