use chrono::{TimeZone, Utc};
use discord_transcript::domain::ai_memory::{AiMemoryNote, AiMemorySourceType, AiMemoryTag};
use discord_transcript::domain::confidence::ConfidencePermille;
use discord_transcript::domain::feedback::{
    TranscriptFeedback, TranscriptFeedbackStatus, TranscriptFeedbackTermType, TranscriptFeedbackType,
};
use discord_transcript::domain::person_alias::{
    PersonAlias, PersonAliasReviewStatus, PersonAliasSourceType,
};
use discord_transcript::infrastructure::sql::{INCREMENTAL_MIGRATIONS_SQL, MIGRATIONS};

fn ai_memory_feedback_migration_sql() -> &'static str {
    MIGRATIONS
        .iter()
        .find(|migration| migration.version == "0021_ai_memory_feedback")
        .expect("ai memory migration should be registered")
        .sql
}

fn sql_between<'a>(sql: &'a str, start: &str, end: &str) -> &'a str {
    sql.split_once(start)
        .expect("start marker should be present")
        .1
        .split_once(end)
        .expect("end marker should be present")
        .0
}

#[test]
fn migrations_register_ai_memory_feedback_schema_after_plan_quotas() {
    let version = MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();

    let plan_quota_position = version
        .iter()
        .position(|version| *version == "0020_plans_and_quotas")
        .expect("plan quota migration should be registered");
    let ai_memory_position = version
        .iter()
        .position(|version| *version == "0021_ai_memory_feedback")
        .expect("ai memory migration should be registered");

    assert!(ai_memory_position > plan_quota_position);
}

#[test]
fn incremental_migrations_include_ai_memory_feedback_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS ai_memory_notes"));
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS transcript_feedback"));
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS person_aliases"));
    assert!(schema.contains("tenant_discord_guild_id TEXT NOT NULL"));
    assert!(schema.contains("tenant_id TEXT NOT NULL REFERENCES tenants(id)"));
    assert!(schema.contains("guild_id TEXT NOT NULL"));
    assert!(schema.contains("idx_tenant_discord_guilds_id_tenant_guild"));
    assert!(schema.contains("REFERENCES tenant_discord_guilds(id, tenant_id, guild_id)"));
    assert!(schema.contains("REFERENCES domain_knowledge_items(id, tenant_id, guild_id)"));
    assert!(schema.contains("REFERENCES ai_memory_notes(id, tenant_id, guild_id)"));
    assert!(schema.contains("idx_ai_memory_notes_tenant_guild_active"));
    assert!(schema.contains("idx_ai_memory_notes_id_tenant_guild"));
    assert!(schema.contains("idx_ai_memory_notes_tags_gin"));
    assert!(schema.contains("idx_transcript_feedback_tenant_guild_status"));
    assert!(schema.contains("idx_transcript_feedback_id_tenant_guild"));
    assert!(schema.contains("idx_person_aliases_tenant_guild_active"));
    assert!(schema.contains("idx_person_aliases_active_identity"));
    assert!(schema.contains("idx_person_aliases_source_feedback"));
}

#[test]
fn ai_memory_feedback_migration_uses_idempotent_statements() {
    let sql = ai_memory_feedback_migration_sql();

    assert!(sql.contains("CREATE TABLE IF NOT EXISTS ai_memory_notes"));
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS transcript_feedback"));
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS person_aliases"));
    assert!(sql.contains("CREATE UNIQUE INDEX IF NOT EXISTS idx_meetings_id_guild"));
    assert!(sql.contains("CREATE INDEX IF NOT EXISTS idx_person_aliases_tenant_guild_active"));
    assert!(sql.contains("ALTER TABLE ai_memory_notes"));
    assert!(sql.contains("EXCEPTION\n    WHEN duplicate_object THEN NULL"));
    assert!(!sql.contains("DROP TABLE"));
    assert!(!sql.contains("DROP COLUMN"));
}

#[test]
fn schema_keeps_ai_memory_and_aliases_separate_from_domain_knowledge() {
    let schema = ai_memory_feedback_migration_sql();

    assert!(schema.contains("target_domain_knowledge_id TEXT"));
    assert!(schema.contains("transcript_feedback_target_domain_fk"));
    assert!(schema.contains("transcript_feedback_target_exclusive_check"));
    assert!(!schema.contains("INSERT INTO domain_knowledge_items"));
    assert!(!schema.contains("ALTER TABLE domain_knowledge_items"));
    assert!(!schema.contains("person_aliases_domain_knowledge"));
    assert!(!schema.contains("ai_memory_notes_domain_knowledge"));
}

#[test]
fn schema_constrains_source_confidence_active_archive_and_review_fields() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("ai_memory_notes_source_type_check"));
    assert!(schema.contains("'ai_meeting_extraction'"));
    assert!(schema.contains("'user_feedback'"));
    assert!(schema.contains("'promotion_candidate'"));
    assert!(schema.contains("ai_memory_notes_confidence_check"));
    assert!(schema.contains("person_aliases_confidence_check"));
    assert!(schema.contains("confidence >= 0.000 AND confidence <= 1.000"));
    assert!(schema.contains("ai_memory_notes_archive_active_check"));
    assert!(schema.contains("person_aliases_archive_active_check"));
    assert!(schema.contains("transcript_feedback_review_actor_check"));
    assert!(schema.contains("person_aliases_review_actor_check"));
    assert!(schema.contains("ai_memory_notes_source_reference_check"));
    assert!(schema.contains("person_aliases_source_reference_check"));
    assert!(schema.contains("archived_at IS NULL OR active = FALSE"));
    assert!(schema.contains("source_type = 'user_feedback'"));
    assert!(schema.contains("source_feedback_id IS NOT NULL"));
    assert!(schema.contains("source_type = 'vc_participant'"));
    assert!(schema.contains("source_meeting_id IS NOT NULL"));
    assert!(schema.contains("source_meeting_id IS NULL"));
    assert!(schema.contains("source_feedback_id IS NULL"));
    assert!(schema.contains("archived_at IS NOT NULL"));
    assert!(schema.contains("reviewed_actor_user_id IS NOT NULL"));
    assert!(schema.contains("archived_actor_user_id IS NOT NULL"));
    assert!(schema.contains("length(btrim(reviewed_actor_user_id)) > 0"));
    assert!(schema.contains("length(btrim(archived_actor_user_id)) > 0"));
}

#[test]
fn schema_constrains_feedback_kinds_and_targets() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("transcript_feedback_type_check"));
    assert!(schema.contains("'mistranscription'"));
    assert!(schema.contains("'speaker'"));
    assert!(schema.contains("'term'"));
    assert!(schema.contains("'person_alias'"));
    assert!(schema.contains("'domain_knowledge'"));
    assert!(schema.contains("'ai_memory'"));
    assert!(schema.contains("transcript_feedback_mistranscription_text_required_check"));
    assert!(schema.contains("transcript_feedback_speaker_required_check"));
    assert!(schema.contains("transcript_feedback_person_alias_required_check"));
    assert!(schema.contains("COALESCE(length(btrim(original_text)) > 0, FALSE)"));
    assert!(schema.contains("COALESCE(length(btrim(corrected_speaker_id)) > 0, FALSE)"));
    assert!(schema.contains("transcript_feedback_term_type_required_check"));
    assert!(schema.contains("transcript_feedback_target_domain_check"));
    assert!(schema.contains("transcript_feedback_target_ai_memory_check"));
    assert!(schema.contains("transcript_feedback_converted_domain_target_check"));
    assert!(schema.contains("transcript_feedback_converted_ai_memory_target_check"));
    assert!(schema.contains("transcript_feedback_target_exclusive_check"));
    assert!(schema.contains("transcript_feedback_segment_meeting_check"));
}

#[test]
fn schema_scopes_meeting_and_segment_references_to_guild_and_meeting() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("idx_meetings_id_guild"));
    assert!(schema.contains("idx_transcripts_id_meeting"));
    assert!(schema.contains("FOREIGN KEY (source_meeting_id, guild_id) REFERENCES meetings(id, guild_id)"));
    let meeting_fk = sql_between(
        schema,
        "CONSTRAINT transcript_feedback_meeting_fk",
        "CONSTRAINT transcript_feedback_meeting_delete_fk",
    );
    assert!(meeting_fk.contains("FOREIGN KEY (meeting_id, guild_id)"));
    assert!(meeting_fk.contains("REFERENCES meetings(id, guild_id)"));
    assert!(meeting_fk.contains("DEFERRABLE INITIALLY DEFERRED"));
    let segment_fk = sql_between(
        schema,
        "CONSTRAINT transcript_feedback_segment_fk",
        "CONSTRAINT transcript_feedback_segment_delete_fk",
    );
    assert!(segment_fk.contains("FOREIGN KEY (transcript_segment_id, meeting_id)"));
    assert!(segment_fk.contains("REFERENCES transcripts(id, meeting_id)"));
    assert!(segment_fk.contains("DEFERRABLE INITIALLY DEFERRED"));
    assert!(schema.contains("transcript_feedback_meeting_delete_fk"));
    assert!(schema.contains("FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE SET NULL"));
    assert!(schema.contains("transcript_feedback_segment_delete_fk"));
    assert!(schema.contains("FOREIGN KEY (transcript_segment_id) REFERENCES transcripts(id) ON DELETE SET NULL"));
    assert!(schema.contains("CREATE OR REPLACE FUNCTION clear_transcript_feedback_meeting_refs()"));
    assert!(schema.contains("DROP TRIGGER IF EXISTS trg_clear_transcript_feedback_meeting_refs ON meetings"));
    assert!(schema.contains("CREATE TRIGGER trg_clear_transcript_feedback_meeting_refs"));
    assert!(schema.contains("SET meeting_id = NULL,\n        transcript_segment_id = NULL"));
    assert!(
        schema.contains("REFERENCES transcript_feedback(id, tenant_id, guild_id) ON DELETE SET NULL")
    );
    assert!(schema.contains("FOREIGN KEY (tenant_discord_guild_id, tenant_id, guild_id)"));
    assert!(schema.contains("FOREIGN KEY (source_feedback_id, tenant_id, guild_id)"));
    assert!(schema.contains("person_aliases_source_reference_check"));
    assert!(schema.contains("idx_person_aliases_active_identity"));
    assert!(schema.contains("ON person_aliases (tenant_id, guild_id, lower(canonical_name), lower(alias))"));
}

#[test]
fn source_feedback_delete_paths_keep_set_null_compatible_with_checks() {
    let schema = ai_memory_feedback_migration_sql();

    assert_eq!(
        schema
            .matches("REFERENCES transcript_feedback(id, tenant_id, guild_id) ON DELETE SET NULL")
            .count(),
        2
    );

    let ai_memory_user_feedback_check = sql_between(
        schema,
        "CONSTRAINT ai_memory_notes_source_reference_check",
        "source_type = 'vc_participant'",
    );
    assert!(ai_memory_user_feedback_check.contains("source_type = 'user_feedback'"));
    assert!(ai_memory_user_feedback_check.contains("source_meeting_id IS NULL"));
    assert!(
        !ai_memory_user_feedback_check.contains("source_feedback_id IS NOT NULL"),
        "ai memory source feedback must be nullable after feedback deletion"
    );

    let alias_user_feedback_check = sql_between(
        schema,
        "CONSTRAINT person_aliases_source_reference_check",
        "source_type = 'vc_participant'",
    );
    assert!(alias_user_feedback_check.contains("source_type = 'user_feedback'"));
    assert!(alias_user_feedback_check.contains("source_meeting_id IS NULL"));
    assert!(
        !alias_user_feedback_check.contains("source_feedback_id IS NOT NULL"),
        "person alias source feedback must be nullable after feedback deletion"
    );
}

#[test]
fn ai_memory_domain_types_match_schema_values() {
    assert_eq!(
        AiMemorySourceType::parse_str("ai_meeting_extraction"),
        Some(AiMemorySourceType::AiMeetingExtraction)
    );
    assert_eq!(
        AiMemorySourceType::PromotionCandidate.as_str(),
        "promotion_candidate"
    );
    assert_eq!(AiMemorySourceType::parse_str("domain_knowledge"), None);
    assert_eq!(AiMemoryTag::parse_str("summary_hint"), Some(AiMemoryTag::SummaryHint));
    assert_eq!(AiMemoryTag::Uncertain.as_str(), "uncertain");
    assert_eq!(
        ConfidencePermille::new(875)
            .expect("valid confidence")
            .as_sql_decimal(),
        "0.875"
    );
    assert_eq!(
        ConfidencePermille::parse_sql_decimal("0.875").expect("valid decimal"),
        ConfidencePermille::new(875).unwrap()
    );
    assert_eq!(
        ConfidencePermille::parse_sql_decimal("1.000").expect("valid full confidence"),
        ConfidencePermille::new(1000).unwrap()
    );
    assert!(ConfidencePermille::new(1001).is_err());
    assert!(ConfidencePermille::parse_sql_decimal("1").is_err());
    assert!(ConfidencePermille::parse_sql_decimal("1.").is_err());
    assert!(ConfidencePermille::parse_sql_decimal("1.001").is_err());
}

#[test]
fn feedback_domain_types_match_schema_values() {
    assert_eq!(
        TranscriptFeedbackType::parse_str("mistranscription"),
        Some(TranscriptFeedbackType::Mistranscription)
    );
    assert_eq!(
        TranscriptFeedbackType::DomainKnowledge.as_str(),
        "domain_knowledge"
    );
    assert_eq!(
        TranscriptFeedbackTermType::parse_str("prohibited_item"),
        Some(TranscriptFeedbackTermType::ProhibitedItem)
    );
    assert_eq!(
        TranscriptFeedbackStatus::parse_str("converted_to_ai_memory"),
        Some(TranscriptFeedbackStatus::ConvertedToAiMemory)
    );
    assert_eq!(TranscriptFeedbackStatus::parse_str("archived"), None);
}

#[test]
fn person_alias_domain_types_match_schema_values() {
    assert_eq!(
        PersonAliasSourceType::parse_str("ai_inference"),
        Some(PersonAliasSourceType::AiInference)
    );
    assert_eq!(PersonAliasSourceType::VcParticipant.as_str(), "vc_participant");
    assert_eq!(
        PersonAliasReviewStatus::parse_str("accepted"),
        Some(PersonAliasReviewStatus::Accepted)
    );
    assert_eq!(PersonAliasReviewStatus::parse_str("open"), None);
}

#[test]
fn domain_models_cover_schema_identity_source_confidence_and_lifecycle_fields() {
    let created_at = Utc.with_ymd_and_hms(2026, 6, 4, 1, 2, 3).unwrap();
    let updated_at = Utc.with_ymd_and_hms(2026, 6, 4, 1, 3, 3).unwrap();
    let reviewed_at = Utc.with_ymd_and_hms(2026, 6, 4, 1, 4, 3).unwrap();
    let archived_at = Utc.with_ymd_and_hms(2026, 6, 4, 1, 5, 3).unwrap();

    let note = AiMemoryNote {
        id: "mem-1".to_owned(),
        tenant_discord_guild_id: "tdg-1".to_owned(),
        tenant_id: "tenant-1".to_owned(),
        guild_id: "guild-1".to_owned(),
        title: "Participant aliases".to_owned(),
        body: "xpa may refer to xpadev.".to_owned(),
        tags: vec![AiMemoryTag::Person, AiMemoryTag::Alias],
        source_type: AiMemorySourceType::UserFeedback,
        source_meeting_id: None,
        source_feedback_id: Some("feedback-1".to_owned()),
        confidence: Some(ConfidencePermille::new(900).unwrap()),
        active: false,
        pinned: true,
        created_actor_user_id: "actor-1".to_owned(),
        updated_actor_user_id: "actor-2".to_owned(),
        last_used_at: Some(updated_at),
        created_at,
        updated_at,
        archived_at: Some(archived_at),
        archived_actor_user_id: Some("actor-3".to_owned()),
    };
    assert_eq!(note.tenant_discord_guild_id, "tdg-1");
    assert_eq!(note.confidence.unwrap().as_permille(), 900);
    assert!(!note.active);
    assert!(note.pinned);

    let feedback = TranscriptFeedback {
        id: "feedback-1".to_owned(),
        tenant_discord_guild_id: "tdg-1".to_owned(),
        tenant_id: "tenant-1".to_owned(),
        guild_id: "guild-1".to_owned(),
        meeting_id: Some("meeting-1".to_owned()),
        transcript_segment_id: Some("segment-1".to_owned()),
        feedback_type: TranscriptFeedbackType::Term,
        term_type: Some(TranscriptFeedbackTermType::PersonName),
        original_text: Some("x p a".to_owned()),
        corrected_text: Some("xpa".to_owned()),
        speaker_id: Some("speaker-1".to_owned()),
        corrected_speaker_id: Some("speaker-2".to_owned()),
        note: Some("Name spelling correction.".to_owned()),
        target_domain_knowledge_id: None,
        target_ai_memory_note_id: Some("mem-1".to_owned()),
        actor_user_id: "actor-1".to_owned(),
        status: TranscriptFeedbackStatus::Accepted,
        created_at,
        reviewed_at: Some(reviewed_at),
        reviewed_actor_user_id: Some("reviewer-1".to_owned()),
    };
    assert_eq!(feedback.tenant_id, "tenant-1");
    assert_eq!(feedback.feedback_type, TranscriptFeedbackType::Term);
    assert_eq!(feedback.status, TranscriptFeedbackStatus::Accepted);
    assert!(feedback.target_domain_knowledge_id.is_none());

    let alias = PersonAlias {
        id: "alias-1".to_owned(),
        tenant_discord_guild_id: "tdg-1".to_owned(),
        tenant_id: "tenant-1".to_owned(),
        guild_id: "guild-1".to_owned(),
        canonical_name: "xpadev".to_owned(),
        alias: "xpa".to_owned(),
        discord_user_id: Some("123".to_owned()),
        source_type: PersonAliasSourceType::UserFeedback,
        source_meeting_id: None,
        source_feedback_id: Some("feedback-1".to_owned()),
        confidence: Some(ConfidencePermille::new(875).unwrap()),
        active: false,
        review_status: PersonAliasReviewStatus::Accepted,
        reviewed_at: Some(reviewed_at),
        reviewed_actor_user_id: Some("reviewer-1".to_owned()),
        archived_at: Some(archived_at),
        archived_actor_user_id: Some("actor-3".to_owned()),
        created_at,
        updated_at,
    };
    assert_eq!(alias.discord_user_id.as_deref(), Some("123"));
    assert_eq!(alias.source_type, PersonAliasSourceType::UserFeedback);
    assert_eq!(alias.review_status, PersonAliasReviewStatus::Accepted);
    assert!(!alias.active);
}
