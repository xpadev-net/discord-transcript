use crate::audio::receiver::BufferedFrame;
use songbird::events::context_data::VoiceTick;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsrcTracker {
    ssrc_to_user: HashMap<u32, String>,
}

impl SsrcTracker {
    pub fn new() -> Self {
        Self {
            ssrc_to_user: HashMap::new(),
        }
    }

    pub fn update_mapping(&mut self, ssrc: u32, user_id: u64) {
        self.ssrc_to_user.insert(ssrc, user_id.to_string());
    }

    pub fn resolve_user(&self, ssrc: u32) -> Option<&str> {
        self.ssrc_to_user.get(&ssrc).map(String::as_str)
    }
}

impl Default for SsrcTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptedVoiceFrames {
    pub per_user: HashMap<String, BufferedFrame>,
}

pub fn adapt_voice_tick(
    tick: &VoiceTick,
    now_ms: u64,
    tracker: &SsrcTracker,
) -> AdaptedVoiceFrames {
    let mut per_user = HashMap::new();
    for (ssrc, voice) in &tick.speaking {
        let Some(decoded) = &voice.decoded_voice else {
            continue;
        };
        if decoded.is_empty() {
            continue;
        }
        let user_id = tracker
            .resolve_user(*ssrc)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("ssrc:{ssrc}"));
        let mono = stereo_to_mono(decoded);
        per_user.insert(
            user_id,
            BufferedFrame {
                timestamp_ms: now_ms,
                pcm_16le_bytes: i16_slice_to_le_bytes(&mono),
            },
        );
    }
    AdaptedVoiceFrames { per_user }
}

fn stereo_to_mono(stereo: &[i16]) -> Vec<i16> {
    let chunks = stereo.chunks_exact(2);
    let remainder = chunks.remainder();
    let mut mono: Vec<i16> = chunks
        .map(|pair| ((pair[0] as i32 + pair[1] as i32) / 2) as i16)
        .collect();
    // If there is a trailing odd sample (incomplete stereo pair), keep it as-is.
    if let Some(&sample) = remainder.first() {
        mono.push(sample);
    }
    mono
}

fn i16_slice_to_le_bytes(input: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 2);
    for sample in input {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}
