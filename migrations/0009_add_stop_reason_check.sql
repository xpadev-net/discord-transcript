DO $$
BEGIN
    ALTER TABLE meetings
    ADD CONSTRAINT meetings_stop_reason_check
    CHECK (
        stop_reason IS NULL
        OR stop_reason IN ('manual', 'auto_empty', 'client_disconnect', 'error')
    );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;
