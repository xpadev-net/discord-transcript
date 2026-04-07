use discord_transcript::asr::StubWhisperClient;
use discord_transcript::bot::{BotCommandService, StartCommandInput, StopCommandInput};
use discord_transcript::command::PermissionSet;
use discord_transcript::config::{AppConfig, ConfigError};
use discord_transcript::domain::{MeetingStatus, StopReason};
use discord_transcript::storage::{InMemoryMeetingStore, StoredMeeting};
use discord_transcript::summary::{SpeakerAudioInput, StubClaudeSummaryClient};
use discord_transcript::worker::{ProcessMeetingInput, process_meeting_summary};
use discord_transcript::workspace::MeetingWorkspaceLayout;
use std::collections::HashMap;
use std::path::PathBuf;

fn temp_workspace(
    meeting_id: &str,
) -> (
    PathBuf,
    discord_transcript::workspace::MeetingWorkspacePaths,
) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!(
        "discord_transcript_runtime_worker_{meeting_id}_{nanos}"
    ));
    let layout = MeetingWorkspaceLayout::new(&base);
    (base, layout.for_meeting("g1", "vc", meeting_id))
}

#[test]
fn app_config_loads_from_map() {
    let mut values = HashMap::new();
    values.insert("DISCORD_TOKEN".to_owned(), "token".to_owned());
    values.insert("DISCORD_GUILD_ID".to_owned(), "guild".to_owned());
    values.insert("WHISPER_ENDPOINT".to_owned(), "http://whisper".to_owned());
    values.insert("CLAUDE_COMMAND".to_owned(), "claude".to_owned());
    values.insert(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/db".to_owned(),
    );
    values.insert("CHUNK_STORAGE_DIR".to_owned(), "/tmp/chunks".to_owned());

    let config = AppConfig::from_map(&values).expect("config should load");
    assert_eq!(config.discord_token, "token");
    assert_eq!(config.discord_guild_id, "guild");
    assert_eq!(config.whisper_endpoint, "http://whisper");
    assert_eq!(config.claude_command, "claude");
    assert_eq!(config.claude_model, "haiku");
    assert_eq!(config.database_url, "postgres://localhost/db");
    assert_eq!(config.database_ssl_mode, "disable");
    assert_eq!(config.chunk_storage_dir, "/tmp/chunks");
    assert_eq!(config.auto_stop_grace_seconds, 60);
    assert_eq!(config.summary_max_retries, 3);
    assert_eq!(config.integration_retry_max_attempts, 3);
    assert_eq!(config.integration_retry_initial_delay_ms, 200);
    assert_eq!(config.integration_retry_backoff_multiplier, 2);
    assert_eq!(config.integration_retry_max_delay_ms, 5_000);
    assert_eq!(config.whisper_language, None);
}

#[test]
fn app_config_accepts_valid_whisper_language() {
    let mut values = HashMap::new();
    values.insert("DISCORD_TOKEN".to_owned(), "token".to_owned());
    values.insert("DISCORD_GUILD_ID".to_owned(), "guild".to_owned());
    values.insert("WHISPER_ENDPOINT".to_owned(), "http://whisper".to_owned());
    values.insert("CLAUDE_COMMAND".to_owned(), "claude".to_owned());
    values.insert(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/db".to_owned(),
    );
    values.insert("CHUNK_STORAGE_DIR".to_owned(), "/tmp/chunks".to_owned());
    values.insert("WHISPER_LANGUAGE".to_owned(), "ja".to_owned());

    let config = AppConfig::from_map(&values).expect("config should load");
    assert_eq!(config.whisper_language, Some("ja".to_owned()));
}

#[test]
fn app_config_accepts_claude_model_override() {
    let mut values = HashMap::new();
    values.insert("DISCORD_TOKEN".to_owned(), "token".to_owned());
    values.insert("DISCORD_GUILD_ID".to_owned(), "guild".to_owned());
    values.insert("WHISPER_ENDPOINT".to_owned(), "http://whisper".to_owned());
    values.insert("CLAUDE_COMMAND".to_owned(), "claude".to_owned());
    values.insert("CLAUDE_MODEL".to_owned(), "sonnet".to_owned());
    values.insert(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/db".to_owned(),
    );
    values.insert("CHUNK_STORAGE_DIR".to_owned(), "/tmp/chunks".to_owned());

    let config = AppConfig::from_map(&values).expect("config should load");
    assert_eq!(config.claude_model, "sonnet");
}

#[test]
fn app_config_rejects_invalid_whisper_language() {
    let mut values = HashMap::new();
    values.insert("DISCORD_TOKEN".to_owned(), "token".to_owned());
    values.insert("DISCORD_GUILD_ID".to_owned(), "guild".to_owned());
    values.insert("WHISPER_ENDPOINT".to_owned(), "http://whisper".to_owned());
    values.insert("CLAUDE_COMMAND".to_owned(), "claude".to_owned());
    values.insert(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/db".to_owned(),
    );
    values.insert("CHUNK_STORAGE_DIR".to_owned(), "/tmp/chunks".to_owned());
    values.insert("WHISPER_LANGUAGE".to_owned(), "Japanese".to_owned());

    let err = AppConfig::from_map(&values).expect_err("config should fail");
    assert_eq!(
        err,
        ConfigError::InvalidEnv {
            key: "WHISPER_LANGUAGE",
            value: "Japanese".to_owned()
        }
    );
}

#[test]
fn app_config_requires_all_values() {
    let values = HashMap::new();
    let err = AppConfig::from_map(&values).expect_err("config should fail");
    assert_eq!(
        err,
        ConfigError::MissingEnv {
            key: "DISCORD_TOKEN"
        }
    );
}

#[test]
fn app_config_loads_retry_overrides_from_map() {
    let mut values = HashMap::new();
    values.insert("DISCORD_TOKEN".to_owned(), "token".to_owned());
    values.insert("DISCORD_GUILD_ID".to_owned(), "guild".to_owned());
    values.insert("WHISPER_ENDPOINT".to_owned(), "http://whisper".to_owned());
    values.insert("CLAUDE_COMMAND".to_owned(), "claude".to_owned());
    values.insert(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/db".to_owned(),
    );
    values.insert("CHUNK_STORAGE_DIR".to_owned(), "/tmp/chunks".to_owned());
    values.insert("SUMMARY_MAX_RETRIES".to_owned(), "5".to_owned());
    values.insert("INTEGRATION_RETRY_MAX_ATTEMPTS".to_owned(), "7".to_owned());
    values.insert(
        "INTEGRATION_RETRY_INITIAL_DELAY_MS".to_owned(),
        "100".to_owned(),
    );
    values.insert(
        "INTEGRATION_RETRY_BACKOFF_MULTIPLIER".to_owned(),
        "3".to_owned(),
    );
    values.insert(
        "INTEGRATION_RETRY_MAX_DELAY_MS".to_owned(),
        "9000".to_owned(),
    );
    values.insert("AUTO_STOP_GRACE_SECONDS".to_owned(), "45".to_owned());

    let config = AppConfig::from_map(&values).expect("config should load");
    assert_eq!(config.summary_max_retries, 5);
    assert_eq!(config.integration_retry_max_attempts, 7);
    assert_eq!(config.integration_retry_initial_delay_ms, 100);
    assert_eq!(config.integration_retry_backoff_multiplier, 3);
    assert_eq!(config.integration_retry_max_delay_ms, 9_000);
    assert_eq!(config.auto_stop_grace_seconds, 45);
}

#[test]
fn app_config_rejects_invalid_retry_override() {
    let mut values = HashMap::new();
    values.insert("DISCORD_TOKEN".to_owned(), "token".to_owned());
    values.insert("DISCORD_GUILD_ID".to_owned(), "guild".to_owned());
    values.insert("WHISPER_ENDPOINT".to_owned(), "http://whisper".to_owned());
    values.insert("CLAUDE_COMMAND".to_owned(), "claude".to_owned());
    values.insert(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/db".to_owned(),
    );
    values.insert("CHUNK_STORAGE_DIR".to_owned(), "/tmp/chunks".to_owned());
    values.insert("SUMMARY_MAX_RETRIES".to_owned(), "abc".to_owned());

    let err = AppConfig::from_map(&values).expect_err("config should fail");
    assert_eq!(
        err,
        ConfigError::InvalidEnv {
            key: "SUMMARY_MAX_RETRIES",
            value: "abc".to_owned()
        }
    );
}

#[test]
fn app_config_rejects_zero_auto_stop_grace() {
    let mut values = HashMap::new();
    values.insert("DISCORD_TOKEN".to_owned(), "token".to_owned());
    values.insert("DISCORD_GUILD_ID".to_owned(), "guild".to_owned());
    values.insert("WHISPER_ENDPOINT".to_owned(), "http://whisper".to_owned());
    values.insert("CLAUDE_COMMAND".to_owned(), "claude".to_owned());
    values.insert(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/db".to_owned(),
    );
    values.insert("CHUNK_STORAGE_DIR".to_owned(), "/tmp/chunks".to_owned());
    values.insert("AUTO_STOP_GRACE_SECONDS".to_owned(), "0".to_owned());

    let err = AppConfig::from_map(&values).expect_err("config should fail");
    assert_eq!(
        err,
        ConfigError::InvalidEnv {
            key: "AUTO_STOP_GRACE_SECONDS",
            value: "0".to_owned()
        }
    );
}

#[test]
fn app_config_supports_optional_ssl_mode() {
    let mut values = HashMap::new();
    values.insert("DISCORD_TOKEN".to_owned(), "token".to_owned());
    values.insert("DISCORD_GUILD_ID".to_owned(), "guild".to_owned());
    values.insert("WHISPER_ENDPOINT".to_owned(), "http://whisper".to_owned());
    values.insert("CLAUDE_COMMAND".to_owned(), "claude".to_owned());
    values.insert(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/db".to_owned(),
    );
    values.insert("DATABASE_SSL_MODE".to_owned(), "require".to_owned());
    values.insert("CHUNK_STORAGE_DIR".to_owned(), "/tmp/chunks".to_owned());

    let config = AppConfig::from_map(&values).expect("config should load");
    assert_eq!(config.database_ssl_mode, "require");
}

#[test]
fn bot_command_service_start_and_stop_flow() {
    let store = InMemoryMeetingStore::new();
    let mut service = BotCommandService::new(store);

    let start_message = service
        .handle_record_start(StartCommandInput {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            user_id: "u1".to_owned(),
            command_channel_id: "c1".to_owned(),
            user_voice_channel_id: Some("vc1".to_owned()),
            permissions: PermissionSet {
                can_connect_voice: true,
                can_send_messages: true,
            },
        })
        .expect("start should pass");
    assert!(start_message.contains("meeting_id=m1"));

    let stop_message = service
        .handle_record_stop(StopCommandInput {
            guild_id: "g1".to_owned(),
            reason: StopReason::Manual,
        })
        .expect("stop should pass");
    assert!(stop_message.contains("outcome=Owner"));
}

#[test]
fn bot_command_service_idempotent_stop() {
    let store = InMemoryMeetingStore::new();
    let mut service = BotCommandService::new(store);

    service
        .handle_record_start(StartCommandInput {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            user_id: "u1".to_owned(),
            command_channel_id: "c1".to_owned(),
            user_voice_channel_id: Some("vc1".to_owned()),
            permissions: PermissionSet {
                can_connect_voice: true,
                can_send_messages: true,
            },
        })
        .expect("start should pass");

    service
        .handle_record_stop(StopCommandInput {
            guild_id: "g1".to_owned(),
            reason: StopReason::Manual,
        })
        .expect("stop should pass");

    // After stop, meeting is Stopping but still found by find_active_meeting_by_guild.
    // The CAS in stop_meeting returns AlreadyHandled, so the command is idempotent.
    let second = service
        .handle_record_stop_result(StopCommandInput {
            guild_id: "g1".to_owned(),
            reason: StopReason::Manual,
        })
        .expect("second stop should succeed (idempotent)");
    assert_eq!(
        second.outcome,
        discord_transcript::stop::StopOutcome::AlreadyHandled,
        "second stop via command should report AlreadyHandled"
    );

    // Direct stop_meeting on the meeting_id is idempotent via CAS
    use discord_transcript::stop::stop_meeting;
    let direct = stop_meeting(&mut service.store, "m1", StopReason::AutoEmpty)
        .expect("direct CAS stop should succeed");
    assert_eq!(
        direct,
        discord_transcript::stop::StopOutcome::AlreadyHandled,
        "CAS stop should report AlreadyHandled"
    );
}

#[test]
fn worker_pipeline_returns_error_without_setting_failed_on_transcription_failure() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc".to_owned(),
        report_channel_id: "c1".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Stopping,
        stop_reason: None,
        error_message: None,
    });

    let whisper = StubWhisperClient {
        mocked_response_json: "{invalid_json".to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "ignored".to_owned(),
    };
    let (base, workspace) = temp_workspace("m1");
    let result = process_meeting_summary(
        &mut store,
        &whisper,
        &claude,
        &ProcessMeetingInput {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc".to_owned(),
            title: None,
            audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
            speaker_audio: vec![SpeakerAudioInput {
                speaker_id: "alice".to_owned(),
                audio_path: "audio.wav".to_owned(),
                offset_ms: 0,
            }],
            language: None,
            workspace: workspace.clone(),
        },
    );

    assert!(result.is_err());
    let saved = store.get("m1").expect("meeting should exist");
    // process_meeting_summary transitions Stopping→Transcribing, transcription fails,
    // then reverts back to Stopping so the next retry's CAS guard succeeds.
    assert_eq!(saved.status, MeetingStatus::Stopping);
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn worker_pipeline_leaves_summarizing_until_posting() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(StoredMeeting {
        id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc".to_owned(),
        report_channel_id: "c1".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Stopping,
        stop_reason: None,
        error_message: None,
    });

    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"ok",
          "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "## Summary\nall good".to_owned(),
    };

    let (base, workspace) = temp_workspace("m1_summary");
    let output = process_meeting_summary(
        &mut store,
        &whisper,
        &claude,
        &ProcessMeetingInput {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc".to_owned(),
            title: None,
            audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
            speaker_audio: vec![SpeakerAudioInput {
                speaker_id: "alice".to_owned(),
                audio_path: "audio.wav".to_owned(),
                offset_ms: 0,
            }],
            language: None,
            workspace: workspace.clone(),
        },
    )
    .expect("worker should succeed");
    assert!(!output.chunks.is_empty());

    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.status, MeetingStatus::Summarizing);
    assert_eq!(saved.error_message, None);
    let _ = std::fs::remove_dir_all(base);
}
