use discord_transcript::audio::receiver::{BufferedFrame, ReceiverConfig};
use discord_transcript::audio::recording_session::RecordingSession;
use discord_transcript::audio::songbird_adapter::SsrcTracker;
use discord_transcript::infrastructure::storage_fs::{
    ChunkStorage, ChunkStorageError, LocalChunkStorage, SavedChunk,
};
use discord_transcript::infrastructure::workspace::{
    MeetingWorkspaceLayout, SSRC_MAPPING_FILENAME,
};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct FlakyChunkStorage {
    base: PathBuf,
    failures_remaining: Arc<AtomicUsize>,
}

impl ChunkStorage for FlakyChunkStorage {
    fn save_chunk(
        &self,
        _meeting_id: &str,
        user_id: &str,
        sequence: u64,
        start_ms: u64,
        bytes: &[u8],
    ) -> Result<SavedChunk, ChunkStorageError> {
        if self
            .failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                if value > 0 { Some(value - 1) } else { None }
            })
            .is_ok()
        {
            return Err(ChunkStorageError::Io("injected failure".to_owned()));
        }
        std::fs::create_dir_all(&self.base).map_err(|err| ChunkStorageError::Io(err.to_string()))?;
        let path = self.base.join(format!("{user_id}_{sequence}_{start_ms}.wav"));
        std::fs::write(&path, bytes).map_err(|err| ChunkStorageError::Io(err.to_string()))?;
        Ok(SavedChunk {
            path,
            size_bytes: bytes.len(),
        })
    }
}

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
    let tmp_files: Vec<_> = std::fs::read_dir(layout.for_meeting("g1", "vc1", "m1").audio_dir())
        .expect("audio dir readable")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"))
        })
        .collect();
    assert!(tmp_files.is_empty(), "temporary chunk files should not remain");

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
            silence_flush_duration: Duration::from_secs(30),
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
fn recording_session_retries_failed_flush_chunks() {
    let base = unique_temp_dir("recording_session_retry_failed");
    let failures_remaining = Arc::new(AtomicUsize::new(1));
    let storage = FlakyChunkStorage {
        base: base.clone(),
        failures_remaining: Arc::clone(&failures_remaining),
    };
    let mut session = RecordingSession::new(
        "meeting-1".to_owned(),
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

    let first = session.flush_all().expect("flush should not fail hard");
    assert_eq!(first.failed.len(), 1);
    assert_eq!(first.newly_failed, 1);
    assert!(first.persisted.is_empty());

    failures_remaining.store(1, Ordering::SeqCst);
    let no_new_chunks = session
        .flush_due(Instant::now() + Duration::from_secs(1))
        .expect("no-op flush should not fail hard");
    assert!(no_new_chunks.failed.is_empty());
    assert_eq!(no_new_chunks.newly_failed, 0);
    assert_eq!(failures_remaining.load(Ordering::SeqCst), 1);

    failures_remaining.store(0, Ordering::SeqCst);
    let second = session.flush_all().expect("retry flush should succeed");
    assert!(second.failed.is_empty());
    assert_eq!(second.newly_failed, 0);
    assert_eq!(second.persisted.len(), 1);
    assert_eq!(second.persisted[0].sequence, 1);
    assert_eq!(second.persisted[0].start_ms, 1_000);
    assert!(second.persisted[0].saved.path.exists());

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn recording_session_rekeys_pending_failed_chunks_before_retry() {
    let base = unique_temp_dir("recording_session_rekey_pending_failed");
    let storage = FlakyChunkStorage {
        base: base.clone(),
        failures_remaining: Arc::new(AtomicUsize::new(1)),
    };
    let mut session = RecordingSession::new(
        "meeting-1".to_owned(),
        storage,
        ReceiverConfig {
            chunk_duration: Duration::from_secs(20),
            silence_flush_duration: Duration::from_secs(30),
            },
        48_000,
    );

    session.ingest_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0, 1, 0],
        },
    );

    let first = session.flush_all().expect("flush should not fail hard");
    assert_eq!(first.failed.len(), 1);
    assert_eq!(first.newly_failed, 1);
    assert_eq!(first.failed[0].user_id, "ssrc:100");

    session.rekey_user("ssrc:100", "u1");

    let second = session.flush_all().expect("retry flush should succeed");
    assert!(second.failed.is_empty());
    assert_eq!(second.persisted.len(), 1);
    assert_eq!(second.persisted[0].user_id, "u1");
    assert!(
        second.persisted[0]
            .saved
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.starts_with("u1_1_1000"))
    );

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
            silence_flush_duration: Duration::from_secs(30),
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
            silence_flush_duration: Duration::from_secs(30),
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
            silence_flush_duration: Duration::from_secs(30),
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

#[test]
fn recording_session_persists_ssrc_mapping_for_rekeyed_pending_failed_chunks() {
    let base = unique_temp_dir("recording_session_pending_failed_mapping");
    let layout = MeetingWorkspaceLayout::new(&base);
    let meeting_dir = layout.for_meeting("g1", "vc1", "meeting-mapping");
    let storage = LocalChunkStorage::new(meeting_dir.clone(), "meeting-mapping");

    std::fs::create_dir_all(meeting_dir.root())
        .expect("meeting root should be creatable");

    // Make the first write fail by blocking `audio/` as a file path.
    let audio_dir = meeting_dir.audio_dir();
    std::fs::write(&audio_dir, b"blocked")
        .expect("setting up blocked audio dir for forced save failure");

    let mut session = RecordingSession::new(
        "meeting-mapping".to_owned(),
        storage,
        ReceiverConfig {
            chunk_duration: Duration::from_secs(20),
            silence_flush_duration: Duration::from_secs(30),
            },
        48_000,
    );

    session.ingest_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0, 1, 0],
        },
    );

    let failed = session.flush_all().expect("flush should return retryable failure");
    assert_eq!(failed.failed.len(), 1);

    assert!(
        std::fs::remove_file(&audio_dir).is_ok(),
        "blocked audio directory should exist as file"
    );
    std::fs::create_dir_all(&audio_dir).expect("recreate audio dir for mapping persist");

    let mut tracker = SsrcTracker::new();
    tracker.update_mapping(100, 12345);
    session.rekey_user("ssrc:100", "12345");
    session.persist_ssrc_mapping(&tracker);

    let mapping_path = meeting_dir.audio_dir().join(SSRC_MAPPING_FILENAME);
    let mapping_data = std::fs::read(&mapping_path).expect("mapping file should be written");
    let parsed: SsrcTracker = serde_json::from_slice(&mapping_data).expect("mapping should parse");
    assert_eq!(parsed.resolve_user(100), Some("12345"));

    let _ = std::fs::remove_dir_all(base);
}
