ALTER TABLE guild_plan_assignments
    ADD COLUMN IF NOT EXISTS period_anchor TIMESTAMPTZ;

UPDATE guild_plan_assignments gpa
SET period_anchor = COALESCE(gpa.period_anchor, t.period_anchor, gpa.valid_from)
FROM tenants t
WHERE t.id = gpa.tenant_id
  AND gpa.period_anchor IS NULL;

UPDATE guild_plan_assignments
SET period_anchor = COALESCE(period_anchor, valid_from, NOW())
WHERE period_anchor IS NULL;

ALTER TABLE guild_plan_assignments
    ALTER COLUMN period_anchor SET NOT NULL;

UPDATE plan_quotas
SET id = 'quota:default:debug_downloads:monthly',
    period = 'monthly',
    updated_at = NOW()
WHERE id = 'quota:default:debug_downloads:daily'
  AND plan_id = 'plan:default'
  AND dimension = 'debug_downloads'
  AND period = 'daily'
  AND NOT EXISTS (
      SELECT 1
      FROM plan_quotas existing
      WHERE existing.id = 'quota:default:debug_downloads:monthly'
  );

UPDATE plan_quotas
SET id = 'quota:beta:debug_downloads:monthly',
    period = 'monthly',
    updated_at = NOW()
WHERE id = 'quota:beta:debug_downloads:daily'
  AND plan_id = 'plan:beta'
  AND dimension = 'debug_downloads'
  AND period = 'daily'
  AND NOT EXISTS (
      SELECT 1
      FROM plan_quotas existing
      WHERE existing.id = 'quota:beta:debug_downloads:monthly'
  );

DROP INDEX IF EXISTS idx_plan_quotas_plan_dimension_period;

CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_quotas_plan_dimension
    ON plan_quotas (plan_id, dimension);

DO $$
BEGIN
    ALTER TABLE transcript_feedback DROP CONSTRAINT IF EXISTS transcript_feedback_meeting_delete_fk;

    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'ai_memory_notes'::regclass
          AND conname = 'ai_memory_notes_archived_actor_check'
          AND pg_get_constraintdef(oid) NOT LIKE '%archived_actor_user_id IS NULL%'
    ) THEN
        ALTER TABLE ai_memory_notes DROP CONSTRAINT ai_memory_notes_archived_actor_check;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'ai_memory_notes'::regclass
          AND conname = 'ai_memory_notes_archived_actor_check'
    ) THEN
        ALTER TABLE ai_memory_notes
        ADD CONSTRAINT ai_memory_notes_archived_actor_check
        CHECK (
            (archived_at IS NULL AND archived_actor_user_id IS NULL)
            OR (
                archived_at IS NOT NULL
                AND archived_actor_user_id IS NOT NULL
                AND length(btrim(archived_actor_user_id)) > 0
            )
        ) NOT VALID;
    END IF;
END
$$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'ai_memory_notes'::regclass
          AND conname = 'ai_memory_notes_source_reference_check'
          AND (
              pg_get_constraintdef(oid) LIKE '%source_meeting_id IS NOT NULL%'
              OR pg_get_constraintdef(oid) LIKE '%source_feedback_id IS NOT NULL%'
          )
    ) THEN
        ALTER TABLE ai_memory_notes DROP CONSTRAINT ai_memory_notes_source_reference_check;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'ai_memory_notes'::regclass
          AND conname = 'ai_memory_notes_source_reference_check'
    ) THEN
        ALTER TABLE ai_memory_notes
        ADD CONSTRAINT ai_memory_notes_source_reference_check
        CHECK (
            (
                source_type = 'ai_meeting_extraction'
                AND source_feedback_id IS NULL
            )
            OR (
                source_type = 'user_feedback'
                AND source_meeting_id IS NULL
            )
            OR (
                source_type = 'vc_participant'
                AND source_feedback_id IS NULL
            )
            OR (
                source_type IN ('manual', 'promotion_candidate')
                AND source_meeting_id IS NULL
                AND source_feedback_id IS NULL
            )
        ) NOT VALID;
    END IF;
END
$$;

DROP INDEX IF EXISTS idx_ai_memory_notes_guild_tags;

CREATE INDEX IF NOT EXISTS idx_ai_memory_notes_tags_gin
    ON ai_memory_notes USING GIN (tags);

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'transcript_feedback'::regclass
          AND conname = 'transcript_feedback_meeting_fk'
          AND pg_get_constraintdef(oid) NOT LIKE '%DEFERRABLE INITIALLY DEFERRED%'
    ) THEN
        ALTER TABLE transcript_feedback DROP CONSTRAINT transcript_feedback_meeting_fk;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'transcript_feedback'::regclass
          AND conname = 'transcript_feedback_meeting_fk'
    ) THEN
        ALTER TABLE transcript_feedback
        ADD CONSTRAINT transcript_feedback_meeting_fk
        FOREIGN KEY (meeting_id, guild_id) REFERENCES meetings(id, guild_id)
        DEFERRABLE INITIALLY DEFERRED NOT VALID;
    END IF;
END
$$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'transcript_feedback'::regclass
          AND conname = 'transcript_feedback_segment_fk'
          AND pg_get_constraintdef(oid) NOT LIKE '%DEFERRABLE INITIALLY DEFERRED%'
    ) THEN
        ALTER TABLE transcript_feedback DROP CONSTRAINT transcript_feedback_segment_fk;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'transcript_feedback'::regclass
          AND conname = 'transcript_feedback_segment_fk'
    ) THEN
        ALTER TABLE transcript_feedback
        ADD CONSTRAINT transcript_feedback_segment_fk
        FOREIGN KEY (transcript_segment_id, meeting_id) REFERENCES transcripts(id, meeting_id)
        DEFERRABLE INITIALLY DEFERRED NOT VALID;
    END IF;
END
$$;

DO $$
BEGIN
    ALTER TABLE transcript_feedback
    ADD CONSTRAINT transcript_feedback_segment_delete_fk
    FOREIGN KEY (transcript_segment_id) REFERENCES transcripts(id) ON DELETE SET NULL
    NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE transcript_feedback
    ADD CONSTRAINT transcript_feedback_mistranscription_text_required_check
    CHECK (
        feedback_type <> 'mistranscription'
        OR COALESCE(length(btrim(original_text)) > 0, FALSE)
        OR COALESCE(length(btrim(corrected_text)) > 0, FALSE)
    ) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE ai_memory_notes DROP CONSTRAINT IF EXISTS ai_memory_notes_source_feedback_fk;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'ai_memory_notes'::regclass
          AND conname = 'ai_memory_notes_source_feedback_scope_fk'
    ) THEN
        ALTER TABLE ai_memory_notes
        ADD CONSTRAINT ai_memory_notes_source_feedback_scope_fk
        FOREIGN KEY (source_feedback_id, tenant_id, guild_id)
        REFERENCES transcript_feedback(id, tenant_id, guild_id)
        DEFERRABLE INITIALLY DEFERRED NOT VALID;
    END IF;
END
$$;

DO $$
BEGIN
    ALTER TABLE ai_memory_notes
    ADD CONSTRAINT ai_memory_notes_source_feedback_delete_fk
    FOREIGN KEY (source_feedback_id)
    REFERENCES transcript_feedback(id) ON DELETE SET NULL
    NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

ALTER TABLE person_aliases
    ADD COLUMN IF NOT EXISTS created_actor_user_id TEXT;

ALTER TABLE person_aliases
    ADD COLUMN IF NOT EXISTS updated_actor_user_id TEXT;

UPDATE person_aliases
SET created_actor_user_id = COALESCE(NULLIF(created_actor_user_id, ''), 'migration'),
    updated_actor_user_id = COALESCE(NULLIF(updated_actor_user_id, ''), 'migration')
WHERE created_actor_user_id IS NULL
   OR created_actor_user_id = ''
   OR updated_actor_user_id IS NULL
   OR updated_actor_user_id = '';

ALTER TABLE person_aliases
    ALTER COLUMN created_actor_user_id SET NOT NULL;

ALTER TABLE person_aliases
    ALTER COLUMN updated_actor_user_id SET NOT NULL;

DO $$
BEGIN
    ALTER TABLE person_aliases DROP CONSTRAINT IF EXISTS person_aliases_source_feedback_fk;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'person_aliases'::regclass
          AND conname = 'person_aliases_source_feedback_scope_fk'
    ) THEN
        ALTER TABLE person_aliases
        ADD CONSTRAINT person_aliases_source_feedback_scope_fk
        FOREIGN KEY (source_feedback_id, tenant_id, guild_id)
        REFERENCES transcript_feedback(id, tenant_id, guild_id)
        DEFERRABLE INITIALLY DEFERRED NOT VALID;
    END IF;
END
$$;

DO $$
BEGIN
    ALTER TABLE person_aliases
    ADD CONSTRAINT person_aliases_source_feedback_delete_fk
    FOREIGN KEY (source_feedback_id)
    REFERENCES transcript_feedback(id) ON DELETE SET NULL
    NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE person_aliases
    ADD CONSTRAINT person_aliases_created_actor_nonempty_check
    CHECK (length(btrim(created_actor_user_id)) > 0) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE person_aliases
    ADD CONSTRAINT person_aliases_updated_actor_nonempty_check
    CHECK (length(btrim(updated_actor_user_id)) > 0) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'person_aliases'::regclass
          AND conname = 'person_aliases_archived_actor_check'
          AND pg_get_constraintdef(oid) NOT LIKE '%archived_actor_user_id IS NULL%'
    ) THEN
        ALTER TABLE person_aliases DROP CONSTRAINT person_aliases_archived_actor_check;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'person_aliases'::regclass
          AND conname = 'person_aliases_archived_actor_check'
    ) THEN
        ALTER TABLE person_aliases
        ADD CONSTRAINT person_aliases_archived_actor_check
        CHECK (
            (archived_at IS NULL AND archived_actor_user_id IS NULL)
            OR (
                archived_at IS NOT NULL
                AND archived_actor_user_id IS NOT NULL
                AND length(btrim(archived_actor_user_id)) > 0
            )
        ) NOT VALID;
    END IF;
END
$$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'person_aliases'::regclass
          AND conname = 'person_aliases_source_reference_check'
          AND (
              pg_get_constraintdef(oid) LIKE '%source_meeting_id IS NOT NULL%'
              OR pg_get_constraintdef(oid) LIKE '%source_feedback_id IS NOT NULL%'
          )
    ) THEN
        ALTER TABLE person_aliases DROP CONSTRAINT person_aliases_source_reference_check;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'person_aliases'::regclass
          AND conname = 'person_aliases_source_reference_check'
    ) THEN
        ALTER TABLE person_aliases
        ADD CONSTRAINT person_aliases_source_reference_check
        CHECK (
            (
                source_type = 'user_feedback'
                AND source_meeting_id IS NULL
            )
            OR (
                source_type = 'vc_participant'
                AND source_feedback_id IS NULL
            )
            OR (
                source_type IN ('manual', 'ai_inference')
                AND source_meeting_id IS NULL
                AND source_feedback_id IS NULL
            )
        ) NOT VALID;
    END IF;
END
$$;

CREATE OR REPLACE FUNCTION clear_ai_feedback_meeting_refs()
RETURNS TRIGGER AS $$
BEGIN
    UPDATE transcript_feedback
    SET meeting_id = NULL,
        transcript_segment_id = NULL
    WHERE meeting_id = OLD.id
      AND guild_id = OLD.guild_id;

    UPDATE ai_memory_notes
    SET source_meeting_id = NULL
    WHERE source_meeting_id = OLD.id
      AND guild_id = OLD.guild_id;

    UPDATE person_aliases
    SET source_meeting_id = NULL
    WHERE source_meeting_id = OLD.id
      AND guild_id = OLD.guild_id;

    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_clear_transcript_feedback_meeting_refs ON meetings;
DROP TRIGGER IF EXISTS trg_clear_ai_feedback_meeting_refs ON meetings;

DROP FUNCTION IF EXISTS clear_transcript_feedback_meeting_refs();

CREATE TRIGGER trg_clear_ai_feedback_meeting_refs
BEFORE DELETE ON meetings
FOR EACH ROW
EXECUTE FUNCTION clear_ai_feedback_meeting_refs();

CREATE INDEX IF NOT EXISTS idx_person_aliases_guild_source_meeting
    ON person_aliases (guild_id, source_meeting_id)
    WHERE source_meeting_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_person_aliases_source_feedback
    ON person_aliases (source_feedback_id)
    WHERE source_feedback_id IS NOT NULL;

DO $$
BEGIN
    ALTER TABLE transcript_feedback
    ADD CONSTRAINT transcript_feedback_speaker_required_check
    CHECK (
        feedback_type <> 'speaker'
        OR COALESCE(length(btrim(speaker_id)) > 0, FALSE)
        OR COALESCE(length(btrim(corrected_speaker_id)) > 0, FALSE)
    ) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE transcript_feedback
    ADD CONSTRAINT transcript_feedback_person_alias_required_check
    CHECK (
        feedback_type <> 'person_alias'
        OR COALESCE(length(btrim(original_text)) > 0, FALSE)
        OR COALESCE(length(btrim(corrected_text)) > 0, FALSE)
        OR COALESCE(length(btrim(speaker_id)) > 0, FALSE)
        OR COALESCE(length(btrim(corrected_speaker_id)) > 0, FALSE)
    ) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE transcript_feedback
    ADD CONSTRAINT transcript_feedback_converted_domain_target_check
    CHECK (
        status <> 'converted_to_domain_knowledge'
        OR target_domain_knowledge_id IS NOT NULL
    ) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE transcript_feedback
    ADD CONSTRAINT transcript_feedback_converted_ai_memory_target_check
    CHECK (
        status <> 'converted_to_ai_memory'
        OR target_ai_memory_note_id IS NOT NULL
    ) NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;
