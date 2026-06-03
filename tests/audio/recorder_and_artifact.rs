use discord_transcript::audio::receiver::{
    BufferedFrame, MAX_RECEIVER_BUFFERED_FRAMES_PER_USER, MAX_RECEIVER_FRAME_BYTES,
    MAX_RECEIVER_USERS, ReceiverConfig, ReceiverState,
};
use discord_transcript::audio::wav::MAX_WAV_CHUNK_PCM_BYTES;
use discord_transcript::audio::recorder::RecorderEngine;
use discord_transcript::infrastructure::artifact::{
    ArtifactError, ArtifactPolicy, build_transcript_artifact,
};
use std::time::{Duration, Instant};

#[test]
fn recorder_engine_flushes_wav_chunk_when_due() {
    let mut engine = RecorderEngine::new(
        ReceiverConfig {
            chunk_duration: Duration::from_secs(20),
            silence_flush_duration: Duration::from_secs(30),
            },
        48_000,
    );

    let start = Instant::now();
    engine.ingest_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0, 1, 0],
        },
    );

    let before_due = engine
        .flush_due(start + Duration::from_millis(19_999))
        .expect("flush should work");
    assert!(before_due.is_empty());

    let due = engine
        .flush_due(start + Duration::from_secs(21))
        .expect("flush should work");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].user_id, "u1");
    assert!(due[0].wav.bytes.starts_with(b"RIFF"));
}

#[test]
fn transcript_artifact_uses_attachment_when_small() {
    let artifact = build_transcript_artifact(
        "hello",
        &ArtifactPolicy {
            attachment_limit_bytes: 1024,
        },
        Some("https://example.com/transcript.txt".to_owned()),
    )
    .expect("artifact should be created");

    assert!(artifact.inline_attachment.is_some());
}

#[test]
fn transcript_artifact_requires_link_for_large_payload() {
    let err = build_transcript_artifact(
        &"x".repeat(2048),
        &ArtifactPolicy {
            attachment_limit_bytes: 1024,
        },
        None,
    )
    .expect_err("large artifact without link should fail");
    assert_eq!(err, ArtifactError::MissingLink);
}

#[test]
fn receiver_state_rekey_user_moves_frames() {
    let mut state = ReceiverState::default();
    state.track_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 10,
            pcm_16le_bytes: vec![1, 0],
        },
    );
    state.track_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 30,
            pcm_16le_bytes: vec![2, 0],
        },
    );

    let moved = state.rekey_user("ssrc:100", "12345");
    assert_eq!(moved, 2);

    // Old key should be gone; flushing should yield the new key
    let all = state.flush_all_chunks();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].user_id, "12345");
    assert_eq!(all[0].frames.len(), 2);
}

#[test]
fn receiver_state_rekey_user_merges_with_existing() {
    let mut state = ReceiverState::default();
    // Pre-existing frames under the real user ID
    state.track_frame(
        "12345",
        BufferedFrame {
            timestamp_ms: 5,
            pcm_16le_bytes: vec![0, 0],
        },
    );
    // Frames under the SSRC fallback
    state.track_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 10,
            pcm_16le_bytes: vec![1, 0],
        },
    );

    let moved = state.rekey_user("ssrc:100", "12345");
    assert_eq!(moved, 1);

    let all = state.flush_all_chunks();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].user_id, "12345");
    assert_eq!(all[0].frames.len(), 2);
    // Frames should be sorted by timestamp
    assert_eq!(all[0].frames[0].timestamp_ms, 5);
    assert_eq!(all[0].frames[1].timestamp_ms, 10);
}

#[test]
fn receiver_state_rekey_user_noop_for_missing_key() {
    let mut state = ReceiverState::default();
    let moved = state.rekey_user("nonexistent", "12345");
    assert_eq!(moved, 0);
}

#[test]
fn receiver_state_drops_oversized_frames() {
    let mut state = ReceiverState::default();
    state.track_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: 10,
            pcm_16le_bytes: vec![0; MAX_RECEIVER_FRAME_BYTES + 1],
        },
    );

    assert!(state.flush_all_chunks().is_empty());
}

#[test]
fn receiver_state_drops_frames_after_user_limit() {
    let mut state = ReceiverState::default();
    for user_index in 0..=MAX_RECEIVER_USERS {
        state.track_frame(
            &format!("u{user_index}"),
            BufferedFrame {
                timestamp_ms: user_index as u64,
                pcm_16le_bytes: vec![0, 0],
            },
        );
    }

    let chunks = state.flush_all_chunks();
    let rejected_user = format!("u{MAX_RECEIVER_USERS}");
    assert_eq!(chunks.len(), MAX_RECEIVER_USERS);
    assert!(chunks.iter().all(|chunk| chunk.user_id != rejected_user));
}

#[test]
fn receiver_state_frees_user_slots_after_flush() {
    let mut state = ReceiverState::default();
    for user_index in 0..MAX_RECEIVER_USERS {
        state.track_frame(
            &format!("u{user_index}"),
            BufferedFrame {
                timestamp_ms: user_index as u64,
                pcm_16le_bytes: vec![0, 0],
            },
        );
    }

    assert_eq!(state.flush_all_chunks().len(), MAX_RECEIVER_USERS);

    state.track_frame(
        "new-user",
        BufferedFrame {
            timestamp_ms: 10_000,
            pcm_16le_bytes: vec![1, 0],
        },
    );

    let chunks = state.flush_all_chunks();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].user_id, "new-user");
}

#[test]
fn receiver_state_drops_frames_that_would_exceed_assembled_span() {
    let mut state = ReceiverState::default();
    let max_span_ms = (MAX_WAV_CHUNK_PCM_BYTES as u64 / 2) * 1_000 / 48_000;
    state.track_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: 0,
            pcm_16le_bytes: vec![0, 0],
        },
    );
    state.track_frame(
        "u1",
        BufferedFrame {
            timestamp_ms: max_span_ms + 1_000,
            pcm_16le_bytes: vec![1, 0],
        },
    );

    let chunks = state.flush_all_chunks();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].frames.len(), 1);
    assert_eq!(chunks[0].frames[0].timestamp_ms, 0);
}

#[test]
fn receiver_state_rekey_trims_merged_buffer_to_span_limit() {
    let mut state = ReceiverState::default();
    let max_span_ms = (MAX_WAV_CHUNK_PCM_BYTES as u64 / 2) * 1_000 / 48_000;
    state.track_frame(
        "12345",
        BufferedFrame {
            timestamp_ms: 0,
            pcm_16le_bytes: vec![0, 0],
        },
    );
    state.track_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: max_span_ms + 1_000,
            pcm_16le_bytes: vec![1, 0],
        },
    );

    assert_eq!(state.rekey_user("ssrc:100", "12345"), 1);

    let chunks = state.flush_all_chunks();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].frames.len(), 1);
    assert_eq!(chunks[0].frames[0].timestamp_ms, max_span_ms + 1_000);
}

#[test]
fn receiver_state_rekey_preserves_arrival_time_flush_metadata() {
    let mut state = ReceiverState::default();
    let config = ReceiverConfig {
        chunk_duration: Duration::from_secs(60),
        silence_flush_duration: Duration::from_millis(5),
    };

    state.track_frame(
        "12345",
        BufferedFrame {
            timestamp_ms: 1_000,
            pcm_16le_bytes: vec![0, 0],
        },
    );
    std::thread::sleep(Duration::from_millis(20));
    state.track_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: 500,
            pcm_16le_bytes: vec![1, 0],
        },
    );

    assert_eq!(state.rekey_user("ssrc:100", "12345"), 1);
    assert!(
        state.users_ready_to_flush(Instant::now(), &config).is_empty(),
        "silence flush should use the latest retained arrival time, not timestamp sort order"
    );
}

#[test]
fn receiver_state_rekey_trims_merged_buffer_to_frame_limit() {
    let mut state = ReceiverState::default();
    for frame_index in 0..MAX_RECEIVER_BUFFERED_FRAMES_PER_USER {
        state.track_frame(
            "12345",
            BufferedFrame {
                timestamp_ms: frame_index as u64,
                pcm_16le_bytes: vec![0, 0],
            },
        );
    }
    state.track_frame(
        "ssrc:100",
        BufferedFrame {
            timestamp_ms: (MAX_RECEIVER_BUFFERED_FRAMES_PER_USER + 1) as u64,
            pcm_16le_bytes: vec![1, 0],
        },
    );

    assert_eq!(state.rekey_user("ssrc:100", "12345"), 1);

    let chunks = state.flush_all_chunks();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].frames.len(), MAX_RECEIVER_BUFFERED_FRAMES_PER_USER);
    assert_eq!(chunks[0].frames[0].timestamp_ms, 1);
}
