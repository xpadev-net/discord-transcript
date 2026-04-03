use discord_transcript::artifact::{ArtifactError, ArtifactPolicy, build_transcript_artifact};
use discord_transcript::receiver::{BufferedFrame, ReceiverConfig};
use discord_transcript::recorder::RecorderEngine;
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
