use discord_transcript::audio::receiver::{BufferedFrame, ReceiverConfig};
use discord_transcript::audio::recording_session::RecordingSession;
use discord_transcript::infrastructure::storage_fs::{ChunkStorage, LocalChunkStorage};
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
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

#[test]
fn recording_session_rekey_user_transfers_sequence_counter() {
    let base = unique_temp_dir("rekey_seq");
    let layout = MeetingWorkspaceLayout::new(&base);
    let storage =
        LocalChunkStorage::new(layout.for_meeting("g1", "vc1", "meeting-rk"), "meeting-rk");
    let mut session = RecordingSession::new(
        "meeting-rk".to_owned(),
        storage,
        ReceiverConfig {
            chunk_duration: Duration::from_secs(5),
        },
        48_000,
    );

    let start = Instant::now();

    // Ingest frames under the SSRC fallback key and flush to commit sequence
    session.ingest_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0],
        },
    );
    let first = session
        .flush_due(start + Duration::from_secs(6))
        .expect("first flush should succeed");
    assert_eq!(first.persisted.len(), 1);
    assert_eq!(first.persisted[0].user_id, "ssrc:100");
    assert_eq!(first.persisted[0].sequence, 1);

    // Ingest another frame (still under fallback, not yet flushed)
    session.ingest_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 7_000,
            pcm_16le_bytes: vec![1, 0],
        },
    );

    // Re-key: sequence counter (1) should transfer to the real user ID
    let moved = session.rekey_user("ssrc:100", "12345");
    assert_eq!(moved, 1);

    // Flush the remaining frame — should use the new user ID and
    // continue the sequence (2) from the transferred counter
    let second = session
        .flush_due(start + Duration::from_secs(12))
        .expect("second flush should succeed");
    assert_eq!(second.persisted.len(), 1);
    assert_eq!(second.persisted[0].user_id, "12345");
    assert_eq!(second.persisted[0].sequence, 2);

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn recording_session_rekey_user_keeps_higher_sequence() {
    let base = unique_temp_dir("rekey_max");
    let layout = MeetingWorkspaceLayout::new(&base);
    let storage =
        LocalChunkStorage::new(layout.for_meeting("g1", "vc1", "meeting-mx"), "meeting-mx");
    let mut session = RecordingSession::new(
        "meeting-mx".to_owned(),
        storage,
        ReceiverConfig {
            chunk_duration: Duration::from_secs(5),
        },
        48_000,
    );

    let start = Instant::now();

    // Build up sequence 3 under the real user ID
    for i in 0..3u64 {
        session.ingest_frame(
            "12345",
            BufferedFrame {
                timestamp_ms: i * 6_000,
                pcm_16le_bytes: vec![0, 0],
            },
        );
        session
            .flush_due(start + Duration::from_secs((i + 1) * 6))
            .expect("flush should succeed");
    }

    // Build up sequence 1 under SSRC fallback
    session.ingest_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 20_000,
            pcm_16le_bytes: vec![1, 0],
        },
    );
    session
        .flush_due(start + Duration::from_secs(25))
        .expect("flush should succeed");

    // Re-key: new_seq=3 > old_seq=1, so max keeps 3
    session.ingest_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 30_000,
            pcm_16le_bytes: vec![2, 0],
        },
    );
    session.rekey_user("ssrc:100", "12345");

    let result = session
        .flush_due(start + Duration::from_secs(36))
        .expect("flush should succeed");
    assert_eq!(result.persisted.len(), 1);
    assert_eq!(result.persisted[0].user_id, "12345");
    // Should be 4 (max(3,1) + 1), not 2 (1 + 1)
    assert_eq!(result.persisted[0].sequence, 4);

    let _ = std::fs::remove_dir_all(base);
}
