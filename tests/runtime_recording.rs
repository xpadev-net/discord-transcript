use discord_transcript::receiver::BufferedFrame;
use discord_transcript::recording_session::RecordingSession;
use discord_transcript::runtime::ingest_voice_frames_into_session;
use discord_transcript::songbird_adapter::AdaptedVoiceFrames;
use discord_transcript::storage_fs::LocalChunkStorage;
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
    let mut session = RecordingSession::new(
        "meeting-rt".to_owned(),
        LocalChunkStorage::new(&base),
        discord_transcript::receiver::ReceiverConfig {
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

    let _ = std::fs::remove_dir_all(base);
}
