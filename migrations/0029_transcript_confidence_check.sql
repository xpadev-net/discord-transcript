DO $$
BEGIN
    ALTER TABLE transcripts
    ADD CONSTRAINT transcripts_confidence_check
    CHECK (confidence IS NULL OR (confidence >= 0.0 AND confidence <= 1.0)) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;
