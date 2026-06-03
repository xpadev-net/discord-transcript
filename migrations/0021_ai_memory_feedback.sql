CREATE UNIQUE INDEX IF NOT EXISTS idx_meetings_id_guild
    ON meetings (id, guild_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_transcripts_id_meeting
    ON transcripts (id, meeting_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_domain_knowledge_id_tenant_guild
    ON domain_knowledge_items (id, tenant_id, guild_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_tenant_discord_guilds_id_tenant_guild
    ON tenant_discord_guilds (id, tenant_id, guild_id);

CREATE TABLE IF NOT EXISTS ai_memory_notes (
    id TEXT PRIMARY KEY,
    tenant_discord_guild_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    guild_id TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    tags TEXT[] NOT NULL DEFAULT '{}',
    source_type TEXT NOT NULL,
    source_meeting_id TEXT,
    source_feedback_id TEXT,
    confidence NUMERIC(4,3),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    created_actor_user_id TEXT NOT NULL,
    updated_actor_user_id TEXT NOT NULL,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    archived_at TIMESTAMPTZ,
    archived_actor_user_id TEXT,
    CONSTRAINT ai_memory_notes_tenant_guild_fk
        FOREIGN KEY (tenant_discord_guild_id, tenant_id, guild_id)
        REFERENCES tenant_discord_guilds(id, tenant_id, guild_id) ON DELETE RESTRICT,
    CONSTRAINT ai_memory_notes_source_meeting_fk
        FOREIGN KEY (source_meeting_id, guild_id) REFERENCES meetings(id, guild_id) ON DELETE RESTRICT,
    CONSTRAINT ai_memory_notes_guild_id_nonempty_check CHECK (length(btrim(guild_id)) > 0),
    CONSTRAINT ai_memory_notes_title_nonempty_check CHECK (length(btrim(title)) > 0),
    CONSTRAINT ai_memory_notes_body_nonempty_check CHECK (length(btrim(body)) > 0),
    CONSTRAINT ai_memory_notes_created_actor_nonempty_check CHECK (length(btrim(created_actor_user_id)) > 0),
    CONSTRAINT ai_memory_notes_updated_actor_nonempty_check CHECK (length(btrim(updated_actor_user_id)) > 0),
    CONSTRAINT ai_memory_notes_source_type_check CHECK (
        source_type IN (
            'ai_meeting_extraction',
            'user_feedback',
            'manual',
            'vc_participant',
            'promotion_candidate'
        )
    ),
    CONSTRAINT ai_memory_notes_tags_check CHECK (
        tags <@ ARRAY[
            'person',
            'alias',
            'project',
            'product',
            'terminology',
            'decision',
            'team_convention',
            'summary_hint',
            'transcription_hint',
            'uncertain'
        ]::TEXT[]
    ),
    CONSTRAINT ai_memory_notes_confidence_check CHECK (
        confidence IS NULL OR (confidence >= 0.000 AND confidence <= 1.000)
    ),
    CONSTRAINT ai_memory_notes_archived_actor_check CHECK (
        archived_at IS NULL
        OR (archived_actor_user_id IS NOT NULL AND length(btrim(archived_actor_user_id)) > 0)
    ),
    CONSTRAINT ai_memory_notes_archive_active_check CHECK (
        archived_at IS NULL OR active = FALSE
    ),
    CONSTRAINT ai_memory_notes_source_reference_check CHECK (
        (source_type = 'ai_meeting_extraction' AND source_meeting_id IS NOT NULL)
        OR (source_type = 'user_feedback' AND source_feedback_id IS NOT NULL)
        OR (source_type = 'vc_participant' AND source_meeting_id IS NOT NULL)
        OR source_type IN ('manual', 'promotion_candidate')
    )
);

CREATE INDEX IF NOT EXISTS idx_ai_memory_notes_tenant_guild_active
    ON ai_memory_notes (tenant_id, guild_id, active, pinned DESC, updated_at DESC, id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_ai_memory_notes_id_tenant_guild
    ON ai_memory_notes (id, tenant_id, guild_id);

CREATE INDEX IF NOT EXISTS idx_ai_memory_notes_guild_source_meeting
    ON ai_memory_notes (guild_id, source_meeting_id)
    WHERE source_meeting_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_ai_memory_notes_guild_tags
    ON ai_memory_notes USING GIN (tags);

CREATE INDEX IF NOT EXISTS idx_ai_memory_notes_source_feedback
    ON ai_memory_notes (source_feedback_id)
    WHERE source_feedback_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS transcript_feedback (
    id TEXT PRIMARY KEY,
    tenant_discord_guild_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    guild_id TEXT NOT NULL,
    meeting_id TEXT,
    transcript_segment_id TEXT,
    feedback_type TEXT NOT NULL,
    term_type TEXT,
    original_text TEXT,
    corrected_text TEXT,
    speaker_id TEXT,
    corrected_speaker_id TEXT,
    note TEXT,
    target_domain_knowledge_id TEXT,
    target_ai_memory_note_id TEXT,
    actor_user_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reviewed_at TIMESTAMPTZ,
    reviewed_actor_user_id TEXT,
    CONSTRAINT transcript_feedback_tenant_guild_fk
        FOREIGN KEY (tenant_discord_guild_id, tenant_id, guild_id)
        REFERENCES tenant_discord_guilds(id, tenant_id, guild_id) ON DELETE RESTRICT,
    CONSTRAINT transcript_feedback_meeting_fk
        FOREIGN KEY (meeting_id, guild_id) REFERENCES meetings(id, guild_id) ON DELETE CASCADE,
    CONSTRAINT transcript_feedback_segment_fk
        FOREIGN KEY (transcript_segment_id, meeting_id)
        REFERENCES transcripts(id, meeting_id) ON DELETE SET NULL (transcript_segment_id),
    CONSTRAINT transcript_feedback_target_domain_fk
        FOREIGN KEY (target_domain_knowledge_id, tenant_id, guild_id)
        REFERENCES domain_knowledge_items(id, tenant_id, guild_id) ON DELETE RESTRICT,
    CONSTRAINT transcript_feedback_target_ai_memory_fk
        FOREIGN KEY (target_ai_memory_note_id, tenant_id, guild_id)
        REFERENCES ai_memory_notes(id, tenant_id, guild_id) ON DELETE RESTRICT,
    CONSTRAINT transcript_feedback_guild_id_nonempty_check CHECK (length(btrim(guild_id)) > 0),
    CONSTRAINT transcript_feedback_actor_nonempty_check CHECK (length(btrim(actor_user_id)) > 0),
    CONSTRAINT transcript_feedback_type_check CHECK (
        feedback_type IN (
            'mistranscription',
            'speaker',
            'term',
            'person_alias',
            'domain_knowledge',
            'ai_memory'
        )
    ),
    CONSTRAINT transcript_feedback_term_type_check CHECK (
        term_type IS NULL OR term_type IN (
            'general_term',
            'person_name',
            'project_name',
            'product_name',
            'organization',
            'acronym',
            'wording_rule',
            'prohibited_item'
        )
    ),
    CONSTRAINT transcript_feedback_status_check CHECK (
        status IN (
            'open',
            'accepted',
            'dismissed',
            'converted_to_domain_knowledge',
            'converted_to_ai_memory'
        )
    ),
    CONSTRAINT transcript_feedback_segment_meeting_check CHECK (
        transcript_segment_id IS NULL OR meeting_id IS NOT NULL
    ),
    CONSTRAINT transcript_feedback_mistranscription_text_required_check CHECK (
        feedback_type <> 'mistranscription'
        OR COALESCE(length(btrim(original_text)) > 0, FALSE)
        OR COALESCE(length(btrim(corrected_text)) > 0, FALSE)
    ),
    CONSTRAINT transcript_feedback_speaker_required_check CHECK (
        feedback_type <> 'speaker'
        OR COALESCE(length(btrim(speaker_id)) > 0, FALSE)
        OR COALESCE(length(btrim(corrected_speaker_id)) > 0, FALSE)
    ),
    CONSTRAINT transcript_feedback_term_type_required_check CHECK (
        feedback_type <> 'term' OR term_type IS NOT NULL
    ),
    CONSTRAINT transcript_feedback_target_domain_check CHECK (
        feedback_type <> 'domain_knowledge' OR target_domain_knowledge_id IS NOT NULL
    ),
    CONSTRAINT transcript_feedback_target_ai_memory_check CHECK (
        feedback_type <> 'ai_memory' OR target_ai_memory_note_id IS NOT NULL
    ),
    CONSTRAINT transcript_feedback_target_exclusive_check CHECK (
        target_domain_knowledge_id IS NULL OR target_ai_memory_note_id IS NULL
    ),
    CONSTRAINT transcript_feedback_review_actor_check CHECK (
        (status = 'open' AND reviewed_at IS NULL AND reviewed_actor_user_id IS NULL)
        OR (
            status <> 'open'
            AND reviewed_at IS NOT NULL
            AND reviewed_actor_user_id IS NOT NULL
            AND length(btrim(reviewed_actor_user_id)) > 0
        )
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_transcript_feedback_id_tenant_guild
    ON transcript_feedback (id, tenant_id, guild_id);

DO $$
BEGIN
    ALTER TABLE ai_memory_notes
    ADD CONSTRAINT ai_memory_notes_source_feedback_fk
    FOREIGN KEY (source_feedback_id, tenant_id, guild_id)
    REFERENCES transcript_feedback(id, tenant_id, guild_id) ON DELETE SET NULL (source_feedback_id);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS idx_transcript_feedback_tenant_guild_status
    ON transcript_feedback (tenant_id, guild_id, status, created_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_transcript_feedback_meeting_segment
    ON transcript_feedback (meeting_id, transcript_segment_id, created_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_transcript_feedback_type_status
    ON transcript_feedback (guild_id, feedback_type, status, created_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_transcript_feedback_target_domain
    ON transcript_feedback (target_domain_knowledge_id)
    WHERE target_domain_knowledge_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_transcript_feedback_target_ai_memory
    ON transcript_feedback (target_ai_memory_note_id)
    WHERE target_ai_memory_note_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS person_aliases (
    id TEXT PRIMARY KEY,
    tenant_discord_guild_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    guild_id TEXT NOT NULL,
    canonical_name TEXT NOT NULL,
    alias TEXT NOT NULL,
    discord_user_id TEXT,
    source_type TEXT NOT NULL,
    source_meeting_id TEXT,
    source_feedback_id TEXT,
    confidence NUMERIC(4,3),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    review_status TEXT NOT NULL DEFAULT 'unreviewed',
    reviewed_at TIMESTAMPTZ,
    reviewed_actor_user_id TEXT,
    archived_at TIMESTAMPTZ,
    archived_actor_user_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT person_aliases_tenant_guild_fk
        FOREIGN KEY (tenant_discord_guild_id, tenant_id, guild_id)
        REFERENCES tenant_discord_guilds(id, tenant_id, guild_id) ON DELETE RESTRICT,
    CONSTRAINT person_aliases_source_meeting_fk
        FOREIGN KEY (source_meeting_id, guild_id) REFERENCES meetings(id, guild_id) ON DELETE RESTRICT,
    CONSTRAINT person_aliases_source_feedback_fk
        FOREIGN KEY (source_feedback_id, tenant_id, guild_id)
        REFERENCES transcript_feedback(id, tenant_id, guild_id) ON DELETE SET NULL (source_feedback_id),
    CONSTRAINT person_aliases_guild_id_nonempty_check CHECK (length(btrim(guild_id)) > 0),
    CONSTRAINT person_aliases_canonical_name_nonempty_check CHECK (length(btrim(canonical_name)) > 0),
    CONSTRAINT person_aliases_alias_nonempty_check CHECK (length(btrim(alias)) > 0),
    CONSTRAINT person_aliases_source_type_check CHECK (
        source_type IN (
            'user_feedback',
            'ai_inference',
            'vc_participant',
            'manual'
        )
    ),
    CONSTRAINT person_aliases_confidence_check CHECK (
        confidence IS NULL OR (confidence >= 0.000 AND confidence <= 1.000)
    ),
    CONSTRAINT person_aliases_review_status_check CHECK (
        review_status IN ('unreviewed', 'accepted', 'dismissed')
    ),
    CONSTRAINT person_aliases_review_actor_check CHECK (
        (review_status = 'unreviewed' AND reviewed_at IS NULL AND reviewed_actor_user_id IS NULL)
        OR (
            review_status <> 'unreviewed'
            AND reviewed_at IS NOT NULL
            AND reviewed_actor_user_id IS NOT NULL
            AND length(btrim(reviewed_actor_user_id)) > 0
        )
    ),
    CONSTRAINT person_aliases_archived_actor_check CHECK (
        archived_at IS NULL
        OR (archived_actor_user_id IS NOT NULL AND length(btrim(archived_actor_user_id)) > 0)
    ),
    CONSTRAINT person_aliases_archive_active_check CHECK (
        archived_at IS NULL OR active = FALSE
    ),
    CONSTRAINT person_aliases_source_reference_check CHECK (
        (source_type = 'user_feedback' AND source_feedback_id IS NOT NULL)
        OR (source_type = 'vc_participant' AND source_meeting_id IS NOT NULL)
        OR source_type IN ('manual', 'ai_inference')
    )
);

CREATE INDEX IF NOT EXISTS idx_person_aliases_tenant_guild_active
    ON person_aliases (tenant_id, guild_id, active, updated_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_person_aliases_guild_discord_user
    ON person_aliases (guild_id, discord_user_id, active)
    WHERE discord_user_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_person_aliases_guild_alias_active
    ON person_aliases (guild_id, lower(alias), active);

CREATE UNIQUE INDEX IF NOT EXISTS idx_person_aliases_active_identity
    ON person_aliases (tenant_id, guild_id, lower(canonical_name), lower(alias))
    WHERE active;
