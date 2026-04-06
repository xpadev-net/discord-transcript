use discord_transcript::asr::StubWhisperClient;
use discord_transcript::domain::MeetingStatus;
use discord_transcript::meeting_flow::{MeetingFlowInput, run_meeting_flow};
use discord_transcript::receiver::{BufferedFrame, ReceiverConfig};
use discord_transcript::recording_session::RecordingSession;
use discord_transcript::recovery::RecoveryCandidate;
use discord_transcript::retention::{ArtifactRecord, RetentionKind, RetentionPolicy};
use discord_transcript::storage::{InMemoryMeetingStore, StoredMeeting};
use discord_transcript::storage_fs::LocalChunkStorage;
use discord_transcript::summary::StubClaudeSummaryClient;
use discord_transcript::worker::ProcessMeetingInput;
use discord_transcript::workspace::MeetingWorkspaceLayout;
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
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Stopping,
        stop_reason: None,
        error_message: None,
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
    let storage = LocalChunkStorage::new(workspace.clone(), "m1");
    let mut session = RecordingSession::new(
        "m1".to_owned(),
        storage,
        ReceiverConfig {
            chunk_duration: Duration::from_secs(20),
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
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        title: Some("Weekly".to_owned()),
        audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
        language: Some("ja".to_owned()),
        workspace,
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
        discord_transcript::recovery_runner::RecoveryEffect::SummaryRequeued {
            meeting_id: "m1".to_owned()
        }
    );
    assert!(!output.persisted_chunks.is_empty());
    assert!(!output.summary.chunks.is_empty());
    assert_eq!(output.cleanup_candidates.len(), 1);
    let saved = store.get("m1").expect("meeting should exist");
    assert_eq!(saved.status, MeetingStatus::Summarizing);

    let _ = std::fs::remove_dir_all(base);
}
