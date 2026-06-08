use discord_transcript::infrastructure::sql::{
    GET_EFFECTIVE_MEETING_SETTINGS_SQL, INCREMENTAL_MIGRATIONS_SQL,
    UPSERT_EFFECTIVE_MEETING_SETTINGS_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlMeetingStore, sql_row_from_strings,
};
use discord_transcript::infrastructure::storage::{
    CreateMeetingRequest, EffectiveMeetingSettings, GuildSettingsForSnapshot, InMemoryMeetingStore,
    MeetingSettingsDefaults, MeetingStore, StoreError,
};

fn defaults() -> MeetingSettingsDefaults {
    MeetingSettingsDefaults {
        whisper_language: Some("ja".to_owned()),
        whisper_vad: true,
        whisper_beam_size: 5,
        whisper_suppress_non_speech: true,
        whisper_prompt: Some("default prompt".to_owned()),
        whisper_temperature: 0.0,
        whisper_resample_to_16k: true,
        auto_stop_grace_seconds: 60,
        retention_raw_audio_ttl_days: 7,
        retention_transcript_ttl_days: 30,
        retention_summary_ttl_days: Some(90),
        summary_enabled: true,
    }
}

fn custom_snapshot() -> EffectiveMeetingSettings {
    EffectiveMeetingSettings {
        whisper_language: Some("en".to_owned()),
        whisper_vad: false,
        whisper_beam_size: 8,
        whisper_suppress_non_speech: false,
        whisper_prompt: Some("meeting terms".to_owned()),
        whisper_temperature: 0.25,
        whisper_resample_to_16k: false,
        auto_stop_grace_seconds: 120,
        retention_raw_audio_ttl_days: 14,
        retention_transcript_ttl_days: 60,
        retention_summary_ttl_days: None,
        summary_enabled: false,
        summary_template_id: Some("template-1".to_owned()),
        domain_knowledge_version_id: Some("dkv-1".to_owned()),
    }
}

#[test]
fn effective_settings_resolve_uses_defaults_when_guild_settings_missing() {
    let resolved = EffectiveMeetingSettings::resolve(&defaults(), None);

    assert_eq!(resolved.whisper_language.as_deref(), Some("ja"));
    assert!(resolved.whisper_vad);
    assert_eq!(resolved.whisper_beam_size, 5);
    assert_eq!(resolved.whisper_temperature, 0.0);
    assert!(resolved.whisper_resample_to_16k);
    assert_eq!(resolved.auto_stop_grace_seconds, 60);
    assert!(resolved.summary_enabled);
}

#[test]
fn effective_settings_resolve_applies_nullable_guild_overrides() {
    let guild = GuildSettingsForSnapshot {
        whisper_language: Some("en".to_owned()),
        whisper_language_explicit: true,
        whisper_vad: Some(false),
        auto_stop_grace_seconds: Some(300),
        retention_raw_audio_ttl_days: Some(21),
        retention_transcript_ttl_days: Some(90),
        summary_enabled: Some(false),
    };

    let resolved = EffectiveMeetingSettings::resolve(&defaults(), Some(&guild));

    assert_eq!(resolved.whisper_language.as_deref(), Some("en"));
    assert!(!resolved.whisper_vad);
    assert_eq!(resolved.whisper_beam_size, 5);
    assert_eq!(resolved.whisper_temperature, 0.0);
    assert!(resolved.whisper_resample_to_16k);
    assert_eq!(resolved.auto_stop_grace_seconds, 300);
    assert_eq!(resolved.retention_raw_audio_ttl_days, 21);
    assert_eq!(resolved.retention_transcript_ttl_days, 90);
    assert!(!resolved.summary_enabled);
}

#[test]
fn incremental_migrations_include_effective_meeting_settings_schema() {
    let schema = INCREMENTAL_MIGRATIONS_SQL;

    assert!(schema.contains("CREATE TABLE IF NOT EXISTS meeting_effective_settings"));
    assert!(schema.contains("meeting_effective_settings_whisper_beam_size_check"));
    assert!(schema.contains("meeting_effective_settings_whisper_temperature_check"));
    assert!(schema.contains("meeting_effective_settings_auto_stop_grace_seconds_check"));
}

#[test]
fn sql_store_persists_and_reads_effective_meeting_settings_snapshot() {
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        format!("{GET_EFFECTIVE_MEETING_SETTINGS_SQL}|m1"),
        vec![sql_row_from_strings(vec![
            "en".to_owned(),
            "false".to_owned(),
            "8".to_owned(),
            "false".to_owned(),
            "meeting terms".to_owned(),
            "0.25".to_owned(),
            "false".to_owned(),
            "120".to_owned(),
            "14".to_owned(),
            "60".to_owned(),
            "90".to_owned(),
            "false".to_owned(),
            "template-1".to_owned(),
            "dkv-1".to_owned(),
        ])],
    );
    let mut store = SqlMeetingStore::new(executor);

    store
        .upsert_effective_meeting_settings("m1", custom_snapshot())
        .expect("snapshot upsert should execute");
    let snapshot = store
        .get_effective_meeting_settings("m1")
        .expect("snapshot read should parse")
        .expect("snapshot should exist");

    assert_eq!(snapshot.whisper_language.as_deref(), Some("en"));
    assert_eq!(snapshot.whisper_beam_size, 8);
    assert_eq!(snapshot.whisper_temperature, 0.25);
    assert!(!snapshot.whisper_resample_to_16k);
    assert_eq!(snapshot.auto_stop_grace_seconds, 120);
    assert_eq!(snapshot.retention_summary_ttl_days, Some(90));
    assert!(!snapshot.summary_enabled);
    assert_eq!(snapshot.summary_template_id.as_deref(), Some("template-1"));
    assert_eq!(snapshot.domain_knowledge_version_id.as_deref(), Some("dkv-1"));
    assert!(
        store
            .executor
            .executed
            .iter()
            .any(|(sql, _)| sql == UPSERT_EFFECTIVE_MEETING_SETTINGS_SQL)
    );
}

#[test]
fn sql_store_effective_settings_upsert_returns_not_found_for_missing_meeting() {
    let mut executor = FakeSqlExecutor::default();
    executor.execute_result.insert(
        format!("{UPSERT_EFFECTIVE_MEETING_SETTINGS_SQL}|m-missing\u{1f}en\u{1f}false\u{1f}8\u{1f}false\u{1f}meeting terms\u{1f}0.25\u{1f}false\u{1f}120\u{1f}14\u{1f}60\u{1f}\u{1f}false\u{1f}template-1\u{1f}dkv-1"),
        0,
    );
    let mut store = SqlMeetingStore::new(executor);

    let err = store
        .upsert_effective_meeting_settings("m-missing", custom_snapshot())
        .expect_err("missing meeting should return NotFound");

    assert_eq!(
        err,
        StoreError::NotFound {
            meeting_id: "m-missing".to_owned()
        }
    );
}

#[test]
fn existing_meeting_snapshot_is_not_changed_by_later_guild_settings_resolution() {
    let initial = GuildSettingsForSnapshot {
        whisper_language: Some("ja".to_owned()),
        whisper_language_explicit: true,
        whisper_vad: Some(true),
        auto_stop_grace_seconds: Some(60),
        retention_raw_audio_ttl_days: Some(7),
        retention_transcript_ttl_days: Some(30),
        summary_enabled: Some(true),
    };
    let later = GuildSettingsForSnapshot {
        whisper_language: Some("en".to_owned()),
        whisper_language_explicit: true,
        whisper_vad: Some(false),
        auto_stop_grace_seconds: Some(300),
        retention_raw_audio_ttl_days: Some(21),
        retention_transcript_ttl_days: Some(90),
        summary_enabled: Some(false),
    };
    let mut store = InMemoryMeetingStore::new();
    let snapshot = EffectiveMeetingSettings::resolve(&defaults(), Some(&initial));
    store
        .create_meeting_as_recording(CreateMeetingRequest {
            id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc1".to_owned(),
            voice_channel_name: None,
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
            effective_settings: Some(snapshot),
        })
        .expect("meeting should be created");

    let later_resolved = EffectiveMeetingSettings::resolve(&defaults(), Some(&later));
    assert_eq!(later_resolved.whisper_language.as_deref(), Some("en"));
    assert_eq!(later_resolved.auto_stop_grace_seconds, 300);

    let stored = store
        .get_effective_meeting_settings("m1")
        .expect("snapshot read should succeed")
        .expect("snapshot should exist");
    assert_eq!(stored.whisper_language.as_deref(), Some("ja"));
    assert_eq!(stored.auto_stop_grace_seconds, 60);
    assert!(stored.summary_enabled);
}
