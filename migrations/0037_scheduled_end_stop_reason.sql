ALTER TABLE meetings DROP CONSTRAINT IF EXISTS meetings_stop_reason_check;
ALTER TABLE meetings
    ADD CONSTRAINT meetings_stop_reason_check
    CHECK (stop_reason IS NULL OR stop_reason IN ('manual', 'auto_empty', 'client_disconnect', 'error', 'scheduled_end')) NOT VALID;
ALTER TABLE meetings VALIDATE CONSTRAINT meetings_stop_reason_check;
