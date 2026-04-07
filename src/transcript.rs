use crate::speaker::{SpeakerProfile, display_label_for_id};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSegment {
    pub speaker_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub confidence: Option<f32>,
    pub is_noisy: bool,
    /// Number of original segments merged into this one (for weighted confidence).
    pub merged_count: u32,
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

    for segment in input {
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
            merged_count: segment.merged_count,
        };

        if let Some(prev) = normalized.last_mut()
            && can_merge(prev, &normalized_segment)
        {
            prev.end_ms = normalized_segment.end_ms;
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
    prev.speaker_id == next.speaker_id && next.start_ms <= prev.end_ms + 1_000
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
    let mut lines = Vec::with_capacity(segments.len());
    for segment in segments {
        let label = display_label_for_id(speakers, &segment.speaker_id);
        let noise_tag = if segment.is_noisy { " [NOISY]" } else { "" };
        if label == segment.speaker_id {
            lines.push(format!(
                "[{}-{}] {}{}: {}",
                segment.start_ms, segment.end_ms, label, noise_tag, segment.text
            ));
        } else {
            lines.push(format!(
                "[{}-{}] {} (id:{}){}: {}",
                segment.start_ms,
                segment.end_ms,
                label,
                segment.speaker_id,
                noise_tag,
                segment.text
            ));
        }
    }
    lines.join("\n")
}
