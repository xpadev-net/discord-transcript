use std::collections::HashMap;

/// Per-speaker audio file and its offset on the meeting timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerAudioInput {
    pub speaker_id: String,
    pub audio_path: String,
    /// Offset from meeting start in milliseconds to align segments from this
    /// speaker's audio back onto the meeting timeline.
    pub offset_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerProfile {
    pub speaker_id: String,
    pub username: Option<String>,
    pub nickname: Option<String>,
    pub display_name: Option<String>,
}

impl SpeakerProfile {
    /// Returns the most human-friendly label available for this speaker.
    /// Preference order: nickname > display_name > username > speaker_id.
    pub fn display_label(&self) -> String {
        self.nickname
            .as_ref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                self.display_name
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
            })
            .or_else(|| {
                self.username
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
            })
            .cloned()
            .unwrap_or_else(|| self.speaker_id.clone())
    }
}

pub fn display_label_for_id(
    profiles: Option<&HashMap<String, SpeakerProfile>>,
    speaker_id: &str,
) -> String {
    let Some(map) = profiles else {
        return speaker_id.to_owned();
    };
    map.get(speaker_id)
        .map(SpeakerProfile::display_label)
        .unwrap_or_else(|| speaker_id.to_owned())
}
