ALTER TABLE transcripts
ADD COLUMN IF NOT EXISTS source TEXT NOT NULL DEFAULT 'voice';

DO $$
BEGIN
    ALTER TABLE transcripts
    ADD CONSTRAINT transcripts_source_check CHECK (source IN ('voice', 'vc_text'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;
