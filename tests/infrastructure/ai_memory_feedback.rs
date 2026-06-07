use chrono::{TimeZone, Utc};
use discord_transcript::domain::ai_memory::{
    AiMemoryNote, AiMemorySourceType, AiMemoryTag, NewAiMemoryNote, UpdateAiMemoryNote,
};
use discord_transcript::domain::confidence::ConfidencePermille;
use discord_transcript::domain::feedback::{
    NewTranscriptFeedback, TranscriptFeedback, TranscriptFeedbackStatus, TranscriptFeedbackTermType,
    TranscriptFeedbackType, UpdateTranscriptFeedbackStatus,
};
use discord_transcript::domain::person_alias::{
    NewPersonAlias, PersonAlias, PersonAliasReviewStatus, PersonAliasSourceType, UpdatePersonAlias,
};
use discord_transcript::infrastructure::sql::{
    ARCHIVE_AI_MEMORY_NOTE_SQL, ARCHIVE_PERSON_ALIAS_SQL, INCREMENTAL_MIGRATIONS_SQL,
    INSERT_AI_MEMORY_NOTE_SQL, INSERT_PERSON_ALIAS_SQL, INSERT_TRANSCRIPT_FEEDBACK_SQL,
    LIST_AI_MEMORY_NOTES_SQL, LIST_PERSON_ALIASES_SQL, LIST_TRANSCRIPT_FEEDBACK_SQL, MIGRATIONS,
    RESOLVE_SINGLE_ACTIVE_TENANT_GUILD_SQL, SELECT_SCHEMA_MIGRATION_SQL, SET_AI_MEMORY_PINNED_SQL,
    UPDATE_AI_MEMORY_NOTE_SQL, UPDATE_PERSON_ALIAS_SQL, UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlMeetingStore, sql_row_from_strings,
};

fn ai_memory_feedback_migration_sql() -> &'static str {
    MIGRATIONS
        .iter()
        .find(|migration| migration.version == "0021_ai_memory_feedback")
        .expect("ai memory migration should be registered")
        .sql
}

fn forward_fixups_migration_sql() -> &'static str {
    MIGRATIONS
        .iter()
        .find(|migration| migration.version == "0022_forward_fixups_for_0020_0021")
        .expect("forward fixup migration should be registered")
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

fn constraint_block<'a>(sql: &'a str, constraint: &str) -> &'a str {
    sql.split_once(constraint)
        .expect("constraint should be present")
        .1
        .split_once("EXCEPTION")
        .expect("constraint block should handle duplicate_object")
        .0
}

fn ai_memory_row(id: &str, active: bool, pinned: bool, archived_at: Option<&str>) -> Vec<Option<String>> {
    vec![
        Some(id.to_owned()),
        Some("tdg-1".to_owned()),
        Some("tenant-1".to_owned()),
        Some("guild-1".to_owned()),
        Some("Team terms".to_owned()),
        Some("Use project codenames.".to_owned()),
        Some("terminology,summary_hint".to_owned()),
        Some("manual".to_owned()),
        None,
        None,
        Some("0.875".to_owned()),
        Some(active.to_string()),
        Some(pinned.to_string()),
        Some("actor-1".to_owned()),
        Some("actor-2".to_owned()),
        None,
        Some("2026-06-04T01:02:03.000Z".to_owned()),
        Some("2026-06-04T01:03:03.000Z".to_owned()),
        archived_at.map(str::to_owned),
        archived_at.map(|_| "actor-3".to_owned()),
    ]
}

fn feedback_row(id: &str, status: &str) -> Vec<Option<String>> {
    vec![
        Some(id.to_owned()),
        Some("tdg-1".to_owned()),
        Some("tenant-1".to_owned()),
        Some("guild-1".to_owned()),
        Some("meeting-1".to_owned()),
        Some("segment-1".to_owned()),
        Some("term".to_owned()),
        Some("person_name".to_owned()),
        Some("x p a".to_owned()),
        Some("xpa".to_owned()),
        None,
        None,
        Some("note".to_owned()),
        None,
        Some("mem-1".to_owned()),
        Some("actor-1".to_owned()),
        Some(status.to_owned()),
        Some("2026-06-04T01:02:03.000Z".to_owned()),
        if status == "open" {
            None
        } else {
            Some("2026-06-04T01:04:03.000Z".to_owned())
        },
        if status == "open" {
            None
        } else {
            Some("reviewer-1".to_owned())
        },
    ]
}

fn person_alias_row(id: &str, active: bool, review_status: &str, archived_at: Option<&str>) -> Vec<Option<String>> {
    vec![
        Some(id.to_owned()),
        Some("tdg-1".to_owned()),
        Some("tenant-1".to_owned()),
        Some("guild-1".to_owned()),
        Some("xpadev".to_owned()),
        Some("xpa".to_owned()),
        Some("123".to_owned()),
        Some("manual".to_owned()),
        None,
        None,
        Some("0.900".to_owned()),
        Some(active.to_string()),
        Some(review_status.to_owned()),
        Some("actor-1".to_owned()),
        Some("actor-2".to_owned()),
        if review_status == "unreviewed" {
            None
        } else {
            Some("2026-06-04T01:04:03.000Z".to_owned())
        },
        if review_status == "unreviewed" {
            None
        } else {
            Some("reviewer-1".to_owned())
        },
        archived_at.map(str::to_owned),
        archived_at.map(|_| "actor-3".to_owned()),
        Some("2026-06-04T01:02:03.000Z".to_owned()),
        Some("2026-06-04T01:03:03.000Z".to_owned()),
    ]
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
fn migrations_register_forward_fixups_after_ai_memory_feedback() {
    let version = MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();

    let ai_memory_position = version
        .iter()
        .position(|version| *version == "0021_ai_memory_feedback")
        .expect("ai memory migration should be registered");
    let forward_fixup_position = version
        .iter()
        .position(|version| *version == "0022_forward_fixups_for_0020_0021")
        .expect("forward fixup migration should be registered");

    assert!(forward_fixup_position > ai_memory_position);
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
    assert!(schema.contains("idx_person_aliases_guild_source_meeting"));
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
    assert!(schema.contains("person_aliases_created_actor_nonempty_check"));
    assert!(schema.contains("person_aliases_updated_actor_nonempty_check"));
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
fn forward_fixups_migration_adds_feedback_constraints_for_existing_0021_tables() {
    let sql = forward_fixups_migration_sql();

    for constraint in [
        "transcript_feedback_mistranscription_text_required_check",
        "transcript_feedback_speaker_required_check",
        "transcript_feedback_person_alias_required_check",
        "transcript_feedback_converted_domain_target_check",
        "transcript_feedback_converted_ai_memory_target_check",
    ] {
        let block = constraint_block(sql, constraint);
        assert!(block.contains(") NOT VALID"), "{constraint} should be added NOT VALID");
    }
    assert!(sql.contains("ALTER TABLE transcript_feedback"));
    assert!(sql.contains("EXCEPTION\n    WHEN duplicate_object THEN NULL"));
    assert!(!sql.contains("DROP TABLE"));
    assert!(!sql.contains("DROP COLUMN"));
}

#[test]
fn forward_fixups_migration_aligns_old_ai_memory_and_alias_schema() {
    let sql = forward_fixups_migration_sql();

    assert!(sql.contains("DROP INDEX IF EXISTS idx_ai_memory_notes_guild_tags"));
    assert!(sql.contains("CREATE INDEX IF NOT EXISTS idx_ai_memory_notes_tags_gin"));
    assert!(sql.contains("DROP CONSTRAINT IF EXISTS ai_memory_notes_source_feedback_fk"));
    assert!(sql.contains("ai_memory_notes_source_feedback_scope_fk"));
    assert!(sql.contains("ai_memory_notes_source_feedback_delete_fk"));
    assert!(sql.contains("DROP CONSTRAINT IF EXISTS person_aliases_source_feedback_fk"));
    assert!(sql.contains("ADD COLUMN IF NOT EXISTS created_actor_user_id TEXT"));
    assert!(sql.contains("ADD COLUMN IF NOT EXISTS updated_actor_user_id TEXT"));
    assert!(sql.contains("COALESCE(NULLIF(created_actor_user_id, ''), 'migration')"));
    assert!(sql.contains("person_aliases_created_actor_nonempty_check"));
    assert!(sql.contains("person_aliases_updated_actor_nonempty_check"));
    assert!(sql.contains("person_aliases_source_feedback_scope_fk"));
    assert!(sql.contains("person_aliases_source_feedback_delete_fk"));
    assert!(sql.contains("person_aliases_guild_source_meeting"));
    assert!(sql.contains("person_aliases_source_feedback"));
}

#[test]
fn forward_fixups_migration_aligns_old_feedback_reference_cleanup_shape() {
    let sql = forward_fixups_migration_sql();

    assert!(sql.contains("transcript_feedback_meeting_fk"));
    assert!(sql.contains("transcript_feedback_segment_fk"));
    assert!(sql.contains("transcript_feedback_segment_delete_fk"));
    assert!(sql.contains("DROP CONSTRAINT IF EXISTS transcript_feedback_meeting_delete_fk"));
    assert!(sql.contains("DEFERRABLE INITIALLY DEFERRED NOT VALID"));
    assert!(sql.contains("REFERENCES transcripts(id) ON DELETE SET NULL"));
    assert!(sql.contains("CREATE OR REPLACE FUNCTION clear_ai_feedback_meeting_refs()"));
    assert!(sql.contains("DROP TRIGGER IF EXISTS trg_clear_transcript_feedback_meeting_refs"));
    assert!(sql.contains("DROP TRIGGER IF EXISTS trg_clear_ai_feedback_meeting_refs"));
    assert!(sql.contains("DROP FUNCTION IF EXISTS clear_transcript_feedback_meeting_refs()"));
    assert!(sql.contains("CREATE TRIGGER trg_clear_ai_feedback_meeting_refs"));
}

#[test]
fn forward_fixups_apply_when_older_0020_and_0021_versions_are_recorded() {
    let mut executor = FakeSqlExecutor::default();
    for migration in MIGRATIONS {
        if migration.version != "0022_forward_fixups_for_0020_0021" {
            executor.query_rows_result.insert(
                format!("{SELECT_SCHEMA_MIGRATION_SQL}|{}", migration.version),
                vec![sql_row_from_strings(vec!["1".to_owned()])],
            );
        }
    }

    let mut store = SqlMeetingStore::new(executor);
    store
        .apply_pending_migrations()
        .expect("forward fixup migration should apply");

    let applied_sql = store
        .executor
        .executed
        .iter()
        .map(|(sql, _)| sql.as_str())
        .filter(|sql| sql.starts_with("BEGIN;"))
        .collect::<Vec<_>>();

    assert_eq!(applied_sql.len(), 1);
    assert!(applied_sql[0].contains("ALTER COLUMN period_anchor SET NOT NULL"));
    assert!(applied_sql[0].contains("transcript_feedback_person_alias_required_check"));
    assert!(applied_sql[0].contains(
        "INSERT INTO schema_migrations (version) VALUES ('0022_forward_fixups_for_0020_0021')"
    ));
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
        "CONSTRAINT transcript_feedback_segment_fk",
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
    assert!(schema.contains("transcript_feedback_segment_delete_fk"));
    assert!(schema.contains("FOREIGN KEY (transcript_segment_id) REFERENCES transcripts(id) ON DELETE SET NULL"));
    assert!(schema.contains("CREATE OR REPLACE FUNCTION clear_ai_feedback_meeting_refs()"));
    assert!(schema.contains("DROP TRIGGER IF EXISTS trg_clear_transcript_feedback_meeting_refs ON meetings"));
    assert!(schema.contains("DROP TRIGGER IF EXISTS trg_clear_ai_feedback_meeting_refs ON meetings"));
    assert!(schema.contains("CREATE TRIGGER trg_clear_ai_feedback_meeting_refs"));
    assert!(schema.contains("SET meeting_id = NULL,\n        transcript_segment_id = NULL"));
    assert!(schema.contains("WHERE meeting_id = OLD.id\n      AND guild_id = OLD.guild_id"));
    assert!(schema.contains("UPDATE ai_memory_notes\n    SET source_meeting_id = NULL"));
    assert!(schema.contains("UPDATE person_aliases\n    SET source_meeting_id = NULL"));
    assert!(schema.contains("REFERENCES transcript_feedback(id) ON DELETE SET NULL"));
    assert!(schema.contains("REFERENCES transcript_feedback(id, tenant_id, guild_id)\n    DEFERRABLE INITIALLY DEFERRED"));
    assert!(schema.contains("REFERENCES transcript_feedback(id, tenant_id, guild_id)\n        DEFERRABLE INITIALLY DEFERRED"));
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
            .matches("REFERENCES transcript_feedback(id) ON DELETE SET NULL")
            .count(),
        2
    );
    assert!(!schema.contains("REFERENCES transcript_feedback(id, tenant_id, guild_id) ON DELETE SET NULL"));

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
    let ai_memory_meeting_check = sql_between(
        schema,
        "source_type = 'ai_meeting_extraction'",
        "source_type = 'user_feedback'",
    );
    assert!(
        !ai_memory_meeting_check.contains("source_meeting_id IS NOT NULL"),
        "ai memory meeting source must be nullable after meeting deletion"
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
    let alias_meeting_check = sql_between(
        schema,
        "CONSTRAINT person_aliases_source_reference_check",
        "source_type IN ('manual', 'ai_inference')",
    );
    assert!(
        !alias_meeting_check.contains("source_meeting_id IS NOT NULL"),
        "person alias meeting source must be nullable after meeting deletion"
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
    assert!(ConfidencePermille::new(900).unwrap() > ConfidencePermille::new(875).unwrap());
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
        created_actor_user_id: "actor-1".to_owned(),
        updated_actor_user_id: "actor-2".to_owned(),
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
    assert_eq!(alias.created_actor_user_id, "actor-1");
    assert_eq!(alias.updated_actor_user_id, "actor-2");
    assert!(!alias.active);
}

#[test]
fn api_sql_resolves_exactly_one_active_tenant_guild_before_new_resource_access() {
    assert!(RESOLVE_SINGLE_ACTIVE_TENANT_GUILD_SQL.contains("tg.status = 'active'"));
    assert!(RESOLVE_SINGLE_ACTIVE_TENANT_GUILD_SQL.contains("t.status = 'active'"));
    assert!(
        RESOLVE_SINGLE_ACTIVE_TENANT_GUILD_SQL
            .contains("WHERE (SELECT COUNT(*) FROM active_installations) = 1")
    );

    for sql in [
        LIST_AI_MEMORY_NOTES_SQL,
        LIST_TRANSCRIPT_FEEDBACK_SQL,
        LIST_PERSON_ALIASES_SQL,
    ] {
        assert!(sql.contains("WHERE tenant_id = $1"));
        assert!(sql.contains("AND guild_id = $2"));
        assert!(!sql.contains("tenant_id IS NULL"));
    }
}

#[test]
fn api_sql_mutations_scope_by_tenant_and_guild_and_preserve_review_state_machines() {
    for sql in [
        UPDATE_AI_MEMORY_NOTE_SQL,
        SET_AI_MEMORY_PINNED_SQL,
        ARCHIVE_AI_MEMORY_NOTE_SQL,
        UPDATE_PERSON_ALIAS_SQL,
        ARCHIVE_PERSON_ALIAS_SQL,
        UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL,
    ] {
        assert!(sql.contains("AND tenant_id = $2"));
        assert!(sql.contains("AND guild_id = $3"));
    }

    assert!(INSERT_AI_MEMORY_NOTE_SQL.contains("tenant_discord_guild_id"));
    assert!(INSERT_AI_MEMORY_NOTE_SQL.contains("created_actor_user_id, updated_actor_user_id"));
    assert!(ARCHIVE_AI_MEMORY_NOTE_SQL.contains("SET active = FALSE"));
    assert!(SET_AI_MEMORY_PINNED_SQL.contains("SET pinned = $4::TEXT::BOOLEAN"));

    assert!(INSERT_TRANSCRIPT_FEEDBACK_SQL.contains("'open'"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("AND status = 'open'"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("WHEN $4 = 'converted_to_ai_memory' THEN NULL"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("WHEN $4 = 'converted_to_domain_knowledge' THEN NULL"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("reviewed_at = NOW()"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("reviewed_actor_user_id = $7"));

    assert!(INSERT_PERSON_ALIAS_SQL.contains("CASE WHEN $13 = 'unreviewed' THEN NULL ELSE NOW() END"));
    assert!(UPDATE_PERSON_ALIAS_SQL.contains("WHEN $9 = 'unreviewed' THEN NULL"));
    assert!(ARCHIVE_PERSON_ALIAS_SQL.contains("SET active = FALSE"));
}

#[test]
fn api_sql_returns_metadata_needed_for_redacted_audit_without_requiring_sensitive_text() {
    for sql in [
        INSERT_AI_MEMORY_NOTE_SQL,
        UPDATE_AI_MEMORY_NOTE_SQL,
        ARCHIVE_AI_MEMORY_NOTE_SQL,
        INSERT_PERSON_ALIAS_SQL,
        UPDATE_PERSON_ALIAS_SQL,
        ARCHIVE_PERSON_ALIAS_SQL,
        UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL,
    ] {
        assert!(sql.contains("RETURNING id"));
    }

    assert!(LIST_AI_MEMORY_NOTES_SQL.contains("array_to_string(tags, ',') AS tags"));
    assert!(INSERT_AI_MEMORY_NOTE_SQL.contains("source_type"));
    assert!(INSERT_AI_MEMORY_NOTE_SQL.contains("source_meeting_id"));
    assert!(INSERT_AI_MEMORY_NOTE_SQL.contains("source_feedback_id"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("feedback_type"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("meeting_id"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("target_domain_knowledge_id"));
    assert!(UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL.contains("target_ai_memory_note_id"));
}

#[test]
fn sql_store_helpers_cover_ai_memory_feedback_status_and_person_aliases() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!(
            "{INSERT_AI_MEMORY_NOTE_SQL}|mem-1\u{1f}tdg-1\u{1f}tenant-1\u{1f}guild-1\u{1f}Team terms\u{1f}Use project codenames.\u{1f}{{\"terminology\",\"summary_hint\"}}\u{1f}manual\u{1f}\u{1f}\u{1f}0.875\u{1f}true\u{1f}false\u{1f}actor-1"
        ),
        vec![ai_memory_row("mem-1", true, false, None)],
    );
    executor.query_rows_result.insert(
        format!(
            "{UPDATE_AI_MEMORY_NOTE_SQL}|mem-1\u{1f}tenant-1\u{1f}guild-1\u{1f}Updated terms\u{1f}Updated body\u{1f}{{\"terminology\"}}\u{1f}0.900\u{1f}true\u{1f}true\u{1f}actor-2"
        ),
        vec![ai_memory_row("mem-1", true, true, None)],
    );
    executor.query_rows_result.insert(
        format!("{SET_AI_MEMORY_PINNED_SQL}|mem-1\u{1f}tenant-1\u{1f}guild-1\u{1f}true\u{1f}actor-2"),
        vec![ai_memory_row("mem-1", true, true, None)],
    );
    executor.query_rows_result.insert(
        format!("{ARCHIVE_AI_MEMORY_NOTE_SQL}|mem-1\u{1f}tenant-1\u{1f}guild-1\u{1f}actor-3"),
        vec![ai_memory_row(
            "mem-1",
            false,
            true,
            Some("2026-06-04T01:05:03.000Z"),
        )],
    );
    executor.query_rows_result.insert(
        format!(
            "{INSERT_TRANSCRIPT_FEEDBACK_SQL}|fb-1\u{1f}tdg-1\u{1f}tenant-1\u{1f}guild-1\u{1f}meeting-1\u{1f}segment-1\u{1f}term\u{1f}person_name\u{1f}x p a\u{1f}xpa\u{1f}\u{1f}\u{1f}note\u{1f}\u{1f}mem-1\u{1f}actor-1"
        ),
        vec![feedback_row("fb-1", "open")],
    );
    executor.query_rows_result.insert(
        format!(
            "{UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL}|fb-1\u{1f}tenant-1\u{1f}guild-1\u{1f}converted_to_ai_memory\u{1f}\u{1f}mem-1\u{1f}reviewer-1"
        ),
        vec![feedback_row("fb-1", "converted_to_ai_memory")],
    );
    executor.query_rows_result.insert(
        format!(
            "{INSERT_PERSON_ALIAS_SQL}|alias-1\u{1f}tdg-1\u{1f}tenant-1\u{1f}guild-1\u{1f}xpadev\u{1f}xpa\u{1f}123\u{1f}manual\u{1f}\u{1f}\u{1f}0.900\u{1f}true\u{1f}accepted\u{1f}actor-1"
        ),
        vec![person_alias_row("alias-1", true, "accepted", None)],
    );
    executor.query_rows_result.insert(
        format!(
            "{UPDATE_PERSON_ALIAS_SQL}|alias-1\u{1f}tenant-1\u{1f}guild-1\u{1f}xpadev\u{1f}xpa-dev\u{1f}123\u{1f}0.900\u{1f}true\u{1f}accepted\u{1f}actor-2"
        ),
        vec![person_alias_row("alias-1", true, "accepted", None)],
    );
    executor.query_rows_result.insert(
        format!("{ARCHIVE_PERSON_ALIAS_SQL}|alias-1\u{1f}tenant-1\u{1f}guild-1\u{1f}actor-3"),
        vec![person_alias_row(
            "alias-1",
            false,
            "accepted",
            Some("2026-06-04T01:05:03.000Z"),
        )],
    );
    let mut store = SqlMeetingStore::new(executor);

    let created_memory = store
        .create_ai_memory_note(&NewAiMemoryNote {
            id: "mem-1".to_owned(),
            tenant_discord_guild_id: "tdg-1".to_owned(),
            tenant_id: "tenant-1".to_owned(),
            guild_id: "guild-1".to_owned(),
            title: "Team terms".to_owned(),
            body: "Use project codenames.".to_owned(),
            tags: vec![AiMemoryTag::Terminology, AiMemoryTag::SummaryHint],
            source_type: AiMemorySourceType::Manual,
            source_meeting_id: None,
            source_feedback_id: None,
            confidence: Some(ConfidencePermille::new(875).unwrap()),
            active: true,
            pinned: false,
            actor_user_id: "actor-1".to_owned(),
        })
        .expect("memory create should parse");
    assert_eq!(created_memory.tags, vec![AiMemoryTag::Terminology, AiMemoryTag::SummaryHint]);

    let updated_memory = store
        .update_ai_memory_note(&UpdateAiMemoryNote {
            id: "mem-1".to_owned(),
            tenant_id: "tenant-1".to_owned(),
            guild_id: "guild-1".to_owned(),
            title: "Updated terms".to_owned(),
            body: "Updated body".to_owned(),
            tags: vec![AiMemoryTag::Terminology],
            confidence: Some(ConfidencePermille::new(900).unwrap()),
            active: true,
            pinned: true,
            actor_user_id: "actor-2".to_owned(),
        })
        .expect("memory update should parse")
        .expect("memory row should exist");
    assert!(updated_memory.pinned);

    let pinned = store
        .set_ai_memory_pinned("tenant-1", "guild-1", "mem-1", true, "actor-2")
        .expect("pin should parse")
        .expect("pin row should exist");
    assert!(pinned.pinned);

    let archived = store
        .archive_ai_memory_note("tenant-1", "guild-1", "mem-1", "actor-3")
        .expect("archive should parse")
        .expect("archive row should exist");
    assert!(!archived.active);
    assert!(archived.archived_at.is_some());

    let created_feedback = store
        .create_transcript_feedback(&NewTranscriptFeedback {
            id: "fb-1".to_owned(),
            tenant_discord_guild_id: "tdg-1".to_owned(),
            tenant_id: "tenant-1".to_owned(),
            guild_id: "guild-1".to_owned(),
            meeting_id: Some("meeting-1".to_owned()),
            transcript_segment_id: Some("segment-1".to_owned()),
            feedback_type: TranscriptFeedbackType::Term,
            term_type: Some(TranscriptFeedbackTermType::PersonName),
            original_text: Some("x p a".to_owned()),
            corrected_text: Some("xpa".to_owned()),
            speaker_id: None,
            corrected_speaker_id: None,
            note: Some("note".to_owned()),
            target_domain_knowledge_id: None,
            target_ai_memory_note_id: Some("mem-1".to_owned()),
            actor_user_id: "actor-1".to_owned(),
        })
        .expect("feedback create should parse");
    assert_eq!(created_feedback.status, TranscriptFeedbackStatus::Open);

    let reviewed = store
        .update_transcript_feedback_status(&UpdateTranscriptFeedbackStatus {
            id: "fb-1".to_owned(),
            tenant_id: "tenant-1".to_owned(),
            guild_id: "guild-1".to_owned(),
            status: TranscriptFeedbackStatus::ConvertedToAiMemory,
            target_domain_knowledge_id: None,
            target_ai_memory_note_id: Some("mem-1".to_owned()),
            reviewed_actor_user_id: "reviewer-1".to_owned(),
        })
        .expect("feedback review should parse")
        .expect("feedback row should exist");
    assert_eq!(reviewed.status, TranscriptFeedbackStatus::ConvertedToAiMemory);
    assert!(reviewed.reviewed_at.is_some());

    let created_alias = store
        .create_person_alias(&NewPersonAlias {
            id: "alias-1".to_owned(),
            tenant_discord_guild_id: "tdg-1".to_owned(),
            tenant_id: "tenant-1".to_owned(),
            guild_id: "guild-1".to_owned(),
            canonical_name: "xpadev".to_owned(),
            alias: "xpa".to_owned(),
            discord_user_id: Some("123".to_owned()),
            source_type: PersonAliasSourceType::Manual,
            source_meeting_id: None,
            source_feedback_id: None,
            confidence: Some(ConfidencePermille::new(900).unwrap()),
            active: true,
            review_status: PersonAliasReviewStatus::Accepted,
            actor_user_id: "actor-1".to_owned(),
        })
        .expect("alias create should parse");
    assert_eq!(created_alias.review_status, PersonAliasReviewStatus::Accepted);

    let updated_alias = store
        .update_person_alias(&UpdatePersonAlias {
            id: "alias-1".to_owned(),
            tenant_id: "tenant-1".to_owned(),
            guild_id: "guild-1".to_owned(),
            canonical_name: "xpadev".to_owned(),
            alias: "xpa-dev".to_owned(),
            discord_user_id: Some("123".to_owned()),
            confidence: Some(ConfidencePermille::new(900).unwrap()),
            active: true,
            review_status: PersonAliasReviewStatus::Accepted,
            actor_user_id: "actor-2".to_owned(),
        })
        .expect("alias update should parse")
        .expect("alias row should exist");
    assert!(updated_alias.active);

    let archived_alias = store
        .archive_person_alias("tenant-1", "guild-1", "alias-1", "actor-3")
        .expect("alias archive should parse")
        .expect("alias row should exist");
    assert!(!archived_alias.active);
    assert!(archived_alias.archived_at.is_some());
}
