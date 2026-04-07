use discord_transcript::application::runtime::ingest_voice_frames_into_session;
use discord_transcript::audio::receiver::BufferedFrame;
use discord_transcript::audio::recording_session::RecordingSession;
use discord_transcript::audio::songbird_adapter::AdaptedVoiceFrames;
use discord_transcript::infrastructure::storage_fs::LocalChunkStorage;
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

fn unique_temp_dir(test_name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("discord_transcript_runtime_{test_name}_{nanos}"))
}

#[test]
fn ingest_voice_frames_into_session_persists_due_chunks() {
    let base = unique_temp_dir("ingest");
    let layout = MeetingWorkspaceLayout::new(&base);
    let workspace = layout.for_meeting("g1", "vc1", "meeting-rt");
    let mut session = RecordingSession::new(
        "meeting-rt".to_owned(),
        LocalChunkStorage::new(workspace.clone(), "meeting-rt"),
        discord_transcript::audio::receiver::ReceiverConfig {
            // Use zero duration so the chunk flushes immediately upon ingest.
            chunk_duration: Duration::ZERO,
        },
        48_000,
    );

    let mut per_user = HashMap::new();
    per_user.insert(
        "u1".to_owned(),
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0, 1, 0],
        },
    );

    let count = ingest_voice_frames_into_session(&mut session, &AdaptedVoiceFrames { per_user })
        .expect("ingest should succeed");
    assert_eq!(count, 1);
    let persisted_wavs: Vec<_> = std::fs::read_dir(workspace.audio_dir())
        .expect("audio dir should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        })
        .collect();
    assert_eq!(
        persisted_wavs.len(),
        1,
        "a single chunk file should be persisted"
    );
    let chunk_size = std::fs::metadata(&persisted_wavs[0])
        .expect("persisted chunk should be readable")
        .len();
    assert!(chunk_size > 0, "persisted chunk file should not be empty");

    let _ = std::fs::remove_dir_all(base);
}
