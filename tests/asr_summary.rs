use discord_transcript::asr::{StubWhisperClient, parse_whisper_response};
use discord_transcript::summary::{
    StubClaudeSummaryClient, SummaryRequest, build_summary_prompt, run_summary_pipeline,
};
use discord_transcript::transcript::{NormalizationConfig, TranscriptSegment, normalize_segments};

#[test]
fn normalize_segments_merges_speaker_and_marks_noisy() {
    let segments = vec![
        TranscriptSegment {
            speaker_id: "alice".to_owned(),
            start_ms: 0,
            end_ms: 1_000,
            text: "  hello   world ".to_owned(),
            confidence: Some(0.9),
            is_noisy: false,
            merged_count: 1,
        },
        TranscriptSegment {
            speaker_id: "alice".to_owned(),
            start_ms: 1_200,
            end_ms: 2_000,
            text: "next".to_owned(),
            confidence: Some(0.4),
            is_noisy: false,
            merged_count: 1,
        },
        TranscriptSegment {
            speaker_id: "bob".to_owned(),
            start_ms: 2_100,
            end_ms: 2_500,
            text: " ".to_owned(),
            confidence: Some(0.8),
            is_noisy: false,
            merged_count: 1,
        },
    ];

    let normalized = normalize_segments(
        &segments,
        NormalizationConfig {
            min_confidence_for_clean: 0.55,
        },
    );
    assert_eq!(normalized.len(), 1);
    assert_eq!(normalized[0].text, "hello world next");
    assert!(normalized[0].is_noisy);
    // merged_count should be the sum of the input segments' merged_count values
    assert_eq!(normalized[0].merged_count, 2);
    // confidence should be the weighted average: (0.9*1 + 0.4*1) / 2 = 0.65
    let conf = normalized[0].confidence.expect("confidence should be Some");
    assert!((conf - 0.65).abs() < 1e-5, "expected ~0.65, got {conf}");
}

#[test]
fn parse_whisper_response_extracts_segments() {
    let json = r#"{
      "text": "transcript text",
      "segments": [
        { "speaker": "alice", "start": 0.0, "end": 1.2, "text": "hello", "confidence": 0.91 },
        { "start": 1.2, "end": 2.3, "text": "world" }
      ]
    }"#;

    let parsed = parse_whisper_response(json).expect("json should parse");
    assert_eq!(parsed.text, "transcript text");
    assert_eq!(parsed.segments.len(), 2);
    assert_eq!(parsed.segments[0].speaker_id, "alice");
    assert_eq!(parsed.segments[0].start_ms, 0);
    assert_eq!(parsed.segments[0].end_ms, 1_200);
    assert_eq!(parsed.segments[1].speaker_id, "unknown");
}

#[test]
fn summary_pipeline_masks_pii_and_chunks_output() {
    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"raw",
          "segments":[
            {"speaker":"alice","start":0.0,"end":1.0,"text":"Contact me at alice@example.com"},
            {"speaker":"alice","start":1.0,"end":2.0,"text":"or +81 90-1234-5678 @bob"}
          ]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "## Summary\nx".to_owned(),
    };
    let request = SummaryRequest {
        meeting_id: "m1".to_owned(),
        title: Some("Weekly".to_owned()),
        audio_path: "audio.wav".to_owned(),
        language: Some("ja".to_owned()),
    };

    let result =
        run_summary_pipeline(&whisper, &claude, &request).expect("pipeline should succeed");
    assert_eq!(result.meeting_id, "m1");
    assert!(result.transcript_for_summary.contains("[EMAIL_1]"));
    assert!(result.transcript_for_summary.contains("[PHONE_1]"));
    assert!(result.transcript_for_summary.contains("[USER_1]"));
    assert_eq!(result.message_chunks.concat(), result.markdown);
    assert!(result.masking_stats.email_replacements >= 1);
    assert!(result.masking_stats.phone_replacements >= 1);
    assert!(result.masking_stats.mention_replacements >= 1);
}

#[test]
fn prompt_contains_required_sections() {
    let request = SummaryRequest {
        meeting_id: "m1".to_owned(),
        title: None,
        audio_path: "audio.wav".to_owned(),
        language: None,
    };

    let prompt = build_summary_prompt(&request, "masked transcript");
    assert!(prompt.contains("## Summary"));
    assert!(prompt.contains("## Decisions"));
    assert!(prompt.contains("## TODO"));
    assert!(prompt.contains("## Open Questions"));
    assert!(prompt.contains("Meeting ID: m1"));
}
