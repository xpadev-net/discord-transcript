use discord_transcript::audio::receiver::{BufferedFrame, ReceiverConfig, ReceiverState};
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
