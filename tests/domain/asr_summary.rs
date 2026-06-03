use discord_transcript::application::summary::{
    SpeakerAudioInput, StubClaudeSummaryClient, SummaryRequest, TranscriptManifest,
    build_correction_prompt, build_summary_prompt, build_summary_prompt_with_template,
    run_summary_pipeline,
};
use discord_transcript::domain::privacy::MaskingStats;
use discord_transcript::domain::speaker::SpeakerProfile;
use discord_transcript::domain::summary_template::{
    SummaryTemplateValidationError, SummaryTemplateVariables, render_summary_template,
    validate_summary_template,
};
use discord_transcript::domain::transcript::{
    NormalizationConfig, TranscriptSegment, TranscriptSource, normalize_segments, render_for_summary,
};
use discord_transcript::infrastructure::asr::{StubWhisperClient, parse_whisper_response};
use discord_transcript::infrastructure::workspace::{MeetingWorkspaceLayout, MeetingWorkspacePaths};
use std::collections::HashMap;
use std::path::PathBuf;

struct TempWorkspaceGuard {
    base: PathBuf,
    workspace: MeetingWorkspacePaths,
}

impl TempWorkspaceGuard {
    fn workspace(&self) -> &MeetingWorkspacePaths {
        &self.workspace
    }
}

impl Drop for TempWorkspaceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn unique_workspace(test_name: &str, meeting_id: &str) -> TempWorkspaceGuard {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base =
        std::env::temp_dir().join(format!("discord_transcript_summary_{test_name}_{nanos}"));
    let layout = MeetingWorkspaceLayout::new(&base);
    TempWorkspaceGuard {
        workspace: layout.for_meeting("g1", "vc1", meeting_id),
        base,
    }
}

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
            source: TranscriptSource::Voice,
            merged_count: 1,
        },
        TranscriptSegment {
            speaker_id: "alice".to_owned(),
            start_ms: 1_200,
            end_ms: 2_000,
            text: "next".to_owned(),
            confidence: Some(0.4),
            is_noisy: false,
            source: TranscriptSource::Voice,
            merged_count: 1,
        },
        TranscriptSegment {
            speaker_id: "bob".to_owned(),
            start_ms: 2_100,
            end_ms: 2_500,
            text: " ".to_owned(),
            confidence: Some(0.8),
            is_noisy: false,
            source: TranscriptSource::Voice,
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
fn render_for_summary_prefers_speaker_labels() {
    let segment = TranscriptSegment {
        speaker_id: "user-1".to_owned(),
        start_ms: 0,
        end_ms: 1_000,
        text: "hello world".to_owned(),
        confidence: None,
        is_noisy: false,
        source: TranscriptSource::Voice,
        merged_count: 1,
    };

    let mut profiles = HashMap::new();
    profiles.insert(
        "user-1".to_owned(),
        SpeakerProfile {
            speaker_id: "user-1".to_owned(),
            username: Some("alice".to_owned()),
            nickname: Some("Alice W.".to_owned()),
            display_name: Some("Alicia".to_owned()),
        },
    );

    let rendered = render_for_summary(std::slice::from_ref(&segment), Some(&profiles));
    assert!(
        rendered.contains("Alice W. (id:user-1)"),
        "nickname should be preferred in label: {rendered}"
    );

    let fallback = render_for_summary(std::slice::from_ref(&segment), None);
    assert!(
        fallback.contains("user-1:"),
        "speaker_id should be used when metadata is missing: {fallback}"
    );
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
    assert_eq!(parsed.raw_body, json);
}

#[test]
fn parse_whisper_response_rejects_empty_object() {
    let err = parse_whisper_response("{}").expect_err("empty object must be rejected");
    assert!(err.to_string().contains("missing field"));
}

#[test]
fn parse_whisper_response_rejects_missing_segments() {
    let err =
        parse_whisper_response(r#"{"text":"hello"}"#).expect_err("segments field is required");
    assert!(err.to_string().contains("missing field"));
}

#[test]
fn parse_whisper_response_accepts_explicit_empty_segments() {
    let parsed = parse_whisper_response(r#"{"text":"","segments":[]}"#)
        .expect("explicit empty response should be valid");

    assert_eq!(parsed.text, "");
    assert!(parsed.segments.is_empty());
}

#[test]
fn parse_whisper_response_rejects_non_empty_text_without_segments() {
    let err = parse_whisper_response(r#"{"text":"hello","segments":[]}"#)
        .expect_err("non-empty text requires matching segments");
    assert!(err.to_string().contains("non-empty text but no segments"));
}

#[test]
fn parse_whisper_response_rejects_invalid_segment_values() {
    for json in [
        r#"{"text":"bad","segments":[{"start":-1.0,"end":1.0,"text":"x"}]}"#,
        r#"{"text":"bad","segments":[{"start":2.0,"end":1.0,"text":"x"}]}"#,
        r#"{"text":"bad","segments":[{"start":0.0,"end":0.0,"text":"x"}]}"#,
        r#"{"text":"bad","segments":[{"start":0.0001,"end":0.0002,"text":"x"}]}"#,
        r#"{"text":"bad","segments":[{"start":3000000.0,"end":3000001.0,"text":"x"}]}"#,
        r#"{"text":"bad","segments":[{"end":1.0,"text":"x"}]}"#,
        r#"{"text":"bad","segments":[{"start":0.0,"end":1.0}]}"#,
        r#"{"text":"bad","segments":[{"start":0.0,"end":1.0,"text":"   "}]}"#,
    ] {
        let result = parse_whisper_response(json);
        assert!(result.is_err(), "invalid segment unexpectedly parsed: {json}");
    }
}

#[test]
fn build_correction_prompt_includes_japanese_rules_when_language_is_ja() {
    let prompt = build_correction_prompt("hi", Some("ja"));
    assert!(
        prompt.contains("misrecognized kanji"),
        "Japanese rules should be included; got: {prompt}"
    );
    assert!(prompt.contains("hi"));
}

#[test]
fn build_correction_prompt_falls_back_to_generic_rules_for_other_languages() {
    let prompt = build_correction_prompt("hi", Some("en"));
    assert!(
        prompt.contains("Fix misrecognized words"),
        "Generic rules should be used for non-ja languages; got: {prompt}"
    );
    assert!(!prompt.contains("misrecognized kanji"));
}

#[test]
fn build_correction_prompt_returns_empty_string_for_blank_transcript() {
    assert!(build_correction_prompt("", Some("ja")).is_empty());
    assert!(build_correction_prompt("   \n", None).is_empty());
}

#[test]
fn summary_template_validation_accepts_approved_variables() {
    let template = "Read {{ transcript_path }} and {{manifest_path}} in {{language}}.";

    assert_eq!(validate_summary_template(template), Ok(()));
    assert_eq!(
        render_summary_template(
            template,
            &SummaryTemplateVariables {
                transcript_path: "transcript/transcript_masked.md".to_owned(),
                manifest_path: "transcript/manifest.json".to_owned(),
                language: "ja".to_owned(),
                speaker_roster: "Alice".to_owned(),
                domain_context_path: "context/".to_owned(),
            },
        )
        .expect("approved variables should render"),
        "Read transcript/transcript_masked.md and transcript/manifest.json in ja."
    );
}

#[test]
fn summary_template_validation_rejects_unknown_variables() {
    assert_eq!(
        validate_summary_template("Read {{secret_path}}."),
        Err(SummaryTemplateValidationError::UnknownVariable(
            "secret_path".to_owned()
        ))
    );
}

#[test]
fn summary_pipeline_masks_pii_and_chunks_output() {
    let original_email = "alice@example.com";
    let original_phone = "+81 90-1234-5678";
    let original_mention = "@bob";
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
    let temp = unique_workspace("pipeline_masks", "m1");
    let workspace = temp.workspace().clone();
    let request = SummaryRequest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        title: Some("Weekly".to_owned()),
        audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
        speaker_audio: vec![SpeakerAudioInput {
            speaker_id: "alice".to_owned(),
            audio_path: "audio.wav".to_owned(),
            offset_ms: 0,
        }],
        language: Some("ja".to_owned()),
        workspace: workspace.clone(),
    };

    let result =
        run_summary_pipeline(&whisper, &claude, &request).expect("pipeline should succeed");
    assert_eq!(result.meeting_id, "m1");
    assert!(result.transcript_for_summary.contains("[EMAIL_1]"));
    assert!(result.transcript_for_summary.contains("[PHONE_1]"));
    assert!(result.transcript_for_summary.contains("[USER_1]"));
    assert!(!result.transcript_for_summary.contains(original_email));
    assert!(!result.transcript_for_summary.contains(original_phone));
    assert!(!result.transcript_for_summary.contains(original_mention));
    assert_eq!(result.message_chunks.concat(), result.markdown);
    assert!(result.masking_stats.email_replacements >= 1);
    assert!(result.masking_stats.phone_replacements >= 1);
    assert!(result.masking_stats.mention_replacements >= 1);
}

#[test]
fn prompt_contains_required_sections() {
    let temp = unique_workspace("prompt_sections", "m1");
    let workspace = temp.workspace().clone();
    let request = SummaryRequest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        title: None,
        audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
        speaker_audio: vec![],
        language: None,
        workspace,
    };
    let forbidden = "SHOULD_NOT_BE_INLINE";
    let manifest = TranscriptManifest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        language: None,
        masked_transcript_path: format!("transcript/{forbidden}.md"),
        generated_at: "2026-01-01T00:00:00Z".to_owned(),
        masking_stats: MaskingStats {
            mention_replacements: 1,
            email_replacements: 2,
            phone_replacements: 3,
        },
    };
    let prompt = build_summary_prompt(&request, &manifest);
    assert!(prompt.contains("## Summary"));
    assert!(prompt.contains("## Decisions"));
    assert!(prompt.contains("## TODO"));
    assert!(prompt.contains("## Open Questions"));
    assert!(prompt.contains("Meeting ID: m1"));
    assert!(prompt.contains("transcript/transcript_masked.md"));
    assert!(
        prompt.contains("speaker names"),
        "prompt should guide model to retain speaker attribution"
    );
    assert!(!prompt.contains(forbidden));
}

#[test]
fn summary_prompt_template_none_preserves_builtin_default() {
    let temp = unique_workspace("prompt_default", "m1");
    let workspace = temp.workspace().clone();
    let request = SummaryRequest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        title: Some("Weekly".to_owned()),
        audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
        speaker_audio: vec![],
        language: Some("ja".to_owned()),
        workspace,
    };
    let manifest = TranscriptManifest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        language: Some("ja".to_owned()),
        masked_transcript_path: "transcript/transcript_masked.md".to_owned(),
        generated_at: "2026-01-01T00:00:00Z".to_owned(),
        masking_stats: MaskingStats::default(),
    };

    assert_eq!(
        build_summary_prompt_with_template(&request, &manifest, None)
            .expect("default prompt should render"),
        build_summary_prompt(&request, &manifest)
    );
}

#[test]
fn summary_prompt_template_renders_custom_template() {
    let temp = unique_workspace("prompt_custom", "m1");
    let workspace = temp.workspace().clone();
    let request = SummaryRequest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        title: None,
        audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
        speaker_audio: vec![],
        language: Some("en".to_owned()),
        workspace,
    };
    let manifest = TranscriptManifest {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        language: Some("en".to_owned()),
        masked_transcript_path: "transcript/transcript_masked.md".to_owned(),
        generated_at: "2026-01-01T00:00:00Z".to_owned(),
        masking_stats: MaskingStats::default(),
    };

    let prompt = build_summary_prompt_with_template(
        &request,
        &manifest,
        Some("Summarize {{transcript_path}} with {{manifest_path}} in {{language}}."),
    )
    .expect("custom template should render");

    assert_eq!(
        prompt,
        "Summarize transcript/transcript_masked.md with transcript/manifest.json in en."
    );
}
