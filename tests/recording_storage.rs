use discord_transcript::receiver::{BufferedFrame, ReceiverConfig};
use discord_transcript::recording_session::RecordingSession;
use discord_transcript::storage_fs::{ChunkStorage, LocalChunkStorage};
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
    let storage = LocalChunkStorage::new(&base);
    let saved = storage
        .save_chunk("m1", "u1", 1, b"abc")
        .expect("save should succeed");

    assert!(saved.path.exists());
    assert_eq!(saved.size_bytes, 3);
    let loaded = std::fs::read(saved.path).expect("file should be readable");
    assert_eq!(loaded, b"abc");

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn recording_session_flushes_and_persists_wav_chunks() {
    let base = unique_temp_dir("recording_session");
    let storage = LocalChunkStorage::new(&base);
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

    let before = session.flush_due(start + Duration::from_millis(19_999)).expect("flush should succeed");
    assert!(before.is_empty());

    let persisted = session.flush_due(start + Duration::from_secs(21)).expect("flush should succeed");
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].sequence, 1);
    assert!(persisted[0].saved.path.exists());

    let bytes = std::fs::read(&persisted[0].saved.path).expect("saved wav should be readable");
    assert!(bytes.starts_with(b"RIFF"));

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn recording_session_increments_sequence_per_user() {
    let base = unique_temp_dir("sequence");
    let storage = LocalChunkStorage::new(&base);
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
    assert_eq!(first[0].sequence, 1);

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
    assert_eq!(second[0].sequence, 2);

    let _ = std::fs::remove_dir_all(base);
}
