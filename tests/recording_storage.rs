use discord_transcript::receiver::{BufferedFrame, ReceiverConfig};
use discord_transcript::recording_session::RecordingSession;
use discord_transcript::storage_fs::{ChunkStorage, LocalChunkStorage};
use discord_transcript::workspace::MeetingWorkspaceLayout;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn unique_temp_dir(test_name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("discord_transcript_{test_name}_{nanos}"))
}

#[test]
fn local_chunk_storage_writes_expected_file() {
    let base = unique_temp_dir("chunk_storage");
    let layout = MeetingWorkspaceLayout::new(&base);
    let storage = LocalChunkStorage::new(layout.for_meeting("g1", "vc1", "m1"), "m1");
    let saved = storage
        .save_chunk("m1", "u1", 1, 0, b"abc")
        .expect("save should succeed");

    assert!(saved.path.exists());
    assert_eq!(
        saved
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default(),
        "u1_1_0.wav"
    );
    assert_eq!(saved.size_bytes, 3);
    let loaded = std::fs::read(saved.path).expect("file should be readable");
    assert_eq!(loaded, b"abc");

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn recording_session_flushes_and_persists_wav_chunks() {
    let base = unique_temp_dir("recording_session");
    let layout = MeetingWorkspaceLayout::new(&base);
    let storage = LocalChunkStorage::new(layout.for_meeting("g1", "vc1", "meeting-1"), "meeting-1");
    let mut session = RecordingSession::new(
        "meeting-1".to_owned(),
        storage,
        ReceiverConfig {
            chunk_duration: Duration::from_secs(20),
        },
        48_000,
    );

    let start = Instant::now();
    session.ingest_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0, 1, 0],
        },
    );

    let before = session
        .flush_due(start + Duration::from_millis(19_999))
        .expect("flush should succeed");
    assert!(before.persisted.is_empty());

    let result = session
        .flush_due(start + Duration::from_secs(21))
        .expect("flush should succeed");
    assert_eq!(result.persisted.len(), 1);
    assert_eq!(result.persisted[0].sequence, 1);
    assert_eq!(result.persisted[0].start_ms, 1_000);
    assert!(result.persisted[0].saved.path.exists());
    assert!(
        result.persisted[0]
            .saved
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.ends_with("_1_1000.wav"))
    );

    let bytes =
        std::fs::read(&result.persisted[0].saved.path).expect("saved wav should be readable");
    assert!(bytes.starts_with(b"RIFF"));

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn recording_session_increments_sequence_per_user() {
    let base = unique_temp_dir("sequence");
    let layout = MeetingWorkspaceLayout::new(&base);
    let storage = LocalChunkStorage::new(layout.for_meeting("g1", "vc1", "meeting-2"), "meeting-2");
    let mut session = RecordingSession::new(
        "meeting-2".to_owned(),
        storage,
        ReceiverConfig {
            chunk_duration: Duration::from_secs(5),
        },
        48_000,
    );

    let start = Instant::now();
    session.ingest_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0],
        },
    );
    let first = session
        .flush_due(start + Duration::from_secs(6))
        .expect("first flush should succeed");
    assert_eq!(first.persisted[0].sequence, 1);

    session.ingest_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: 7_000,
            pcm_16le_bytes: vec![1, 0],
        },
    );
    let second = session
        .flush_due(start + Duration::from_secs(12))
        .expect("second flush should succeed");
    assert_eq!(second.persisted[0].sequence, 2);
    assert_eq!(second.persisted[0].start_ms, 7_000);

    let _ = std::fs::remove_dir_all(base);
}
