use discord_transcript::application::meeting_flow::{MeetingFlowInput, run_meeting_flow};
use discord_transcript::application::summary::{
    SpeakerAudioInput, StubClaudeSummaryClient, SummaryContextInput,
};
use discord_transcript::application::worker::ProcessMeetingInput;
use discord_transcript::audio::build_wav_bytes_raw;
use discord_transcript::audio::receiver::{BufferedFrame, ReceiverConfig};
use discord_transcript::audio::recording_session::RecordingSession;
use discord_transcript::domain::MeetingStatus;
use discord_transcript::domain::recovery::RecoveryCandidate;
use discord_transcript::domain::retention::{ArtifactRecord, RetentionKind, RetentionPolicy};
use discord_transcript::infrastructure::asr::StubWhisperClient;
use discord_transcript::infrastructure::storage::{InMemoryMeetingStore, StoredMeeting};
use discord_transcript::infrastructure::storage_fs::LocalChunkStorage;
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn unique_temp_dir(test_name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("discord_transcript_flow_{test_name}_{nanos}"))
}

fn stopping_meeting(id: &str) -> StoredMeeting {
    StoredMeeting {
        id: id.to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        report_channel_id: "tc1".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Stopping,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
    }
}

#[test]
fn meeting_flow_runs_recovery_recording_summary_and_retention() {
    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));

    let base = unique_temp_dir("run");
    let layout = MeetingWorkspaceLayout::new(&base);
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("workspace dirs");
    let wav = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).expect("wav should build");
    std::fs::write(workspace.mixdown_path(), wav).expect("mixdown should be written");
    let storage = LocalChunkStorage::new(workspace.clone(), "m1");
    let mut session = RecordingSession::new(
        "m1".to_owned(),
        storage,
        ReceiverConfig {
            chunk_duration: Duration::from_secs(20),
            silence_flush_duration: Duration::from_secs(30),
            },
        48_000,
    );
    session.ingest_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0, 1, 0],
        },
    );

    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"ok",
          "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello alice@example.com"}]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "## Summary\ndone".to_owned(),
    };
    let recovery_candidate = RecoveryCandidate {
        meeting_id: "m1".to_owned(),
        status: discord_transcript::domain::MeetingStatus::Stopping,
        voice_connected: false,
        has_recording_file: true,
    };
    let summary_input = ProcessMeetingInput {
        meeting_id: "m1".to_owned(),
        job_id: Some("summary-m1".to_owned()),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        title: Some("Weekly".to_owned()),
        audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
        speaker_audio: vec![SpeakerAudioInput {
            speaker_id: "alice".to_owned(),
            audio_path: "audio.wav".to_owned(),
            offset_ms: 0,
        }],
        language: Some("ja".to_owned()),
        workspace,
        summary_context: SummaryContextInput::default(),
    };
    let retention_records = [ArtifactRecord {
        kind: RetentionKind::RawAudio,
        created_at_unix_seconds: 0,
    }];

    let start = Instant::now();
    let output = run_meeting_flow(
        &mut store,
        &mut session,
        MeetingFlowInput::new(
            &recovery_candidate,
            start + Duration::from_secs(21),
            &whisper,
            &claude,
            &summary_input,
            &retention_records,
            10 * 86_400,
            RetentionPolicy::default(),
        ),
    )
    .expect("meeting flow should succeed");

    assert_eq!(
        output.recovery_effect,
        discord_transcript::application::recovery_runner::RecoveryEffect::SummaryRequeued {
            meeting_id: "m1".to_owned()
        }
    );
    assert!(!output.persisted_chunks.is_empty());
    assert!(!output.summary.chunks.is_empty());
    assert_eq!(output.cleanup_candidates.len(), 1);
    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.status, MeetingStatus::Summarizing);

    let workspace_for_assert =
        MeetingWorkspaceLayout::new(&base).for_meeting("g1", "vc1", "m1");
    let whisper_response_path = workspace_for_assert.whisper_response_path("alice");
    assert!(
        whisper_response_path.exists(),
        "whisper raw response should be persisted at {whisper_response_path:?}"
    );
    let raw_body =
        std::fs::read_to_string(&whisper_response_path).expect("whisper response readable");
    assert!(raw_body.contains("alice@example.com"));

    let pre_correction_path = workspace_for_assert.pre_correction_transcript_path();
    assert!(
        pre_correction_path.exists(),
        "pre-correction transcript should be persisted at {pre_correction_path:?}"
    );

    let correction_prompt_path = workspace_for_assert.correction_prompt_path();
    assert!(
        correction_prompt_path.exists(),
        "correction prompt should be persisted at {correction_prompt_path:?}"
    );

    let summary_prompt_path = workspace_for_assert.summary_prompt_path();
    assert!(
        summary_prompt_path.exists(),
        "summary prompt should be persisted at {summary_prompt_path:?}"
    );
    let summary_prompt =
        std::fs::read_to_string(&summary_prompt_path).expect("summary prompt readable");
    assert!(summary_prompt.contains("Meeting ID: m1"));

    let _ = std::fs::remove_dir_all(base);
}
