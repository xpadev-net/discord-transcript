use crate::domain::speaker::{SpeakerProfile, display_label_for_id};
use std::cmp::Ordering;
use std::collections::HashMap;

pub const MAX_DB_TIMESTAMP_MS: u64 = i32::MAX as u64;
pub const MIN_TRANSCRIPT_CONFIDENCE: f32 = 0.0;
pub const MAX_TRANSCRIPT_CONFIDENCE: f32 = 1.0;

pub fn is_valid_transcript_confidence(value: f32) -> bool {
    value.is_finite() && (MIN_TRANSCRIPT_CONFIDENCE..=MAX_TRANSCRIPT_CONFIDENCE).contains(&value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptSource {
    Voice,
    VcText,
}

impl TranscriptSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::VcText => "vc_text",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "voice" => Some(Self::Voice),
            "vc_text" => Some(Self::VcText),
            _ => None,
        }
    }

    pub fn order_priority(self) -> u8 {
        match self {
            Self::Voice => 0,
            Self::VcText => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSegment {
    pub speaker_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub confidence: Option<f32>,
    pub is_noisy: bool,
    pub source: TranscriptSource,
    /// Number of original segments merged into this one (for weighted confidence).
    pub merged_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptTimelineOrderKey<'a> {
    pub start_ms: i64,
    pub end_ms: i64,
    pub source: TranscriptSource,
    pub stable_id: Option<&'a str>,
}

impl<'a> TranscriptTimelineOrderKey<'a> {
    pub fn new(
        start_ms: i64,
        end_ms: i64,
        source: TranscriptSource,
        stable_id: Option<&'a str>,
    ) -> Self {
        Self {
            start_ms,
            end_ms,
            source,
            stable_id,
        }
    }
}

pub fn compare_transcript_timeline_order(
    left: TranscriptTimelineOrderKey<'_>,
    right: TranscriptTimelineOrderKey<'_>,
) -> Ordering {
    left.start_ms
        .cmp(&right.start_ms)
        .then(left.end_ms.cmp(&right.end_ms))
        .then(
            left.source
                .order_priority()
                .cmp(&right.source.order_priority()),
        )
        .then_with(|| match (left.stable_id, right.stable_id) {
            (Some(left), Some(right)) => left.cmp(right),
            _ => Ordering::Equal,
        })
}

pub fn sort_transcript_segments(segments: &mut [TranscriptSegment]) {
    segments.sort_by(|left, right| {
        compare_transcript_timeline_order(
            transcript_segment_order_key(left, None),
            transcript_segment_order_key(right, None),
        )
    });
}

pub fn ordered_transcript_segments(input: &[TranscriptSegment]) -> Vec<TranscriptSegment> {
    let mut ordered = input.to_vec();
    sort_transcript_segments(&mut ordered);
    ordered
}

fn transcript_segment_order_key<'a>(
    segment: &TranscriptSegment,
    stable_id: Option<&'a str>,
) -> TranscriptTimelineOrderKey<'a> {
    TranscriptTimelineOrderKey::new(
        order_timestamp_ms(segment.start_ms),
        order_timestamp_ms(segment.end_ms),
        segment.source,
        stable_id,
    )
}

fn order_timestamp_ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizationConfig {
    pub min_confidence_for_clean: f32,
}

impl Default for NormalizationConfig {
    fn default() -> Self {
        Self {
            min_confidence_for_clean: 0.55,
        }
    }
}

pub fn normalize_segments(
    input: &[TranscriptSegment],
    config: NormalizationConfig,
) -> Vec<TranscriptSegment> {
    let mut normalized = Vec::new();
    let ordered = ordered_transcript_segments(input);

    for segment in &ordered {
        let cleaned_text = clean_text(&segment.text);
        if cleaned_text.is_empty() {
            continue;
        }
        if segment.end_ms <= segment.start_ms {
            continue;
        }

        let normalized_segment = TranscriptSegment {
            speaker_id: segment.speaker_id.clone(),
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: cleaned_text,
            confidence: segment.confidence,
            is_noisy: segment.is_noisy
                || segment
                    .confidence
                    .is_some_and(|value| value < config.min_confidence_for_clean),
            source: segment.source,
            merged_count: segment.merged_count,
        };

        if let Some(prev) = normalized.last_mut()
            && can_merge(prev, &normalized_segment)
        {
            prev.end_ms = prev.end_ms.max(normalized_segment.end_ms);
            prev.text.push(' ');
            prev.text.push_str(&normalized_segment.text);
            prev.is_noisy = prev.is_noisy || normalized_segment.is_noisy;
            prev.confidence = merge_confidence(
                prev.confidence,
                prev.merged_count,
                normalized_segment.confidence,
                normalized_segment.merged_count,
            );
            prev.merged_count += normalized_segment.merged_count;
            continue;
        }

        normalized.push(normalized_segment);
    }

    normalized
}

fn can_merge(prev: &TranscriptSegment, next: &TranscriptSegment) -> bool {
    if prev.source == TranscriptSource::VcText || next.source == TranscriptSource::VcText {
        return false;
    }
    prev.source == next.source
        && prev.speaker_id == next.speaker_id
        && next.start_ms <= prev.end_ms + 1_000
}

fn merge_confidence(a: Option<f32>, a_count: u32, b: Option<f32>, b_count: u32) -> Option<f32> {
    match (a, b) {
        (Some(x), Some(y)) => {
            let total = a_count + b_count;
            Some((x * a_count as f32 + y * b_count as f32) / total as f32)
        }
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

fn clean_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(trimmed.len());
    let mut previous_was_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !previous_was_space {
                out.push(' ');
                previous_was_space = true;
            }
        } else {
            out.push(ch);
            previous_was_space = false;
        }
    }
    out
}

pub fn render_for_summary(
    segments: &[TranscriptSegment],
    speakers: Option<&HashMap<String, SpeakerProfile>>,
) -> String {
    let ordered = ordered_transcript_segments(segments);
    let mut lines = Vec::with_capacity(ordered.len());
    for segment in &ordered {
        let label = sanitize_transcript_field(&display_label_for_id(speakers, &segment.speaker_id));
        let speaker_id = sanitize_transcript_field(&segment.speaker_id);
        let text = sanitize_transcript_text(&segment.text);
        let noise_tag = if segment.is_noisy { " [NOISY]" } else { "" };
        let source_tag = if segment.source == TranscriptSource::VcText {
            " [VC_TEXT]"
        } else {
            ""
        };
        if label == speaker_id {
            lines.push(format!(
                "[{}-{}] {}{}{}: {}",
                segment.start_ms, segment.end_ms, label, source_tag, noise_tag, text
            ));
        } else {
            lines.push(format!(
                "[{}-{}] {} (id:{}){}{}: {}",
                segment.start_ms, segment.end_ms, label, speaker_id, source_tag, noise_tag, text
            ));
        }
    }
    lines.join("\n")
}

fn sanitize_transcript_field(value: &str) -> String {
    let normalized = clean_text(value);
    normalized
        .chars()
        .map(|ch| match ch {
            ':' => ';',
            '[' => '(',
            ']' => ')',
            _ => ch,
        })
        .collect()
}

fn sanitize_transcript_text(value: &str) -> String {
    clean_text(value)
}
