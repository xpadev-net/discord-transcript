use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverConfig {
    pub chunk_duration: Duration,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            chunk_duration: Duration::from_secs(20),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferedFrame {
    pub timestamp_ms: u64,
    pub pcm_16le_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAudioBuffer {
    pub user_id: String,
    pub frames: Vec<BufferedFrame>,
    pub first_frame_ms: Option<u64>,
}

impl UserAudioBuffer {
    pub fn new(user_id: String) -> Self {
        Self {
            user_id,
            frames: Vec::new(),
            first_frame_ms: None,
        }
    }

    pub fn push_frame(&mut self, frame: BufferedFrame) {
        if self.first_frame_ms.is_none() {
            self.first_frame_ms = Some(frame.timestamp_ms);
        }
        self.frames.push(frame);
    }

    pub fn should_flush(&self, now_ms: u64, config: &ReceiverConfig) -> bool {
        let Some(first) = self.first_frame_ms else {
            return false;
        };
        now_ms.saturating_sub(first) >= config.chunk_duration.as_millis() as u64
    }

    pub fn take_frames(&mut self) -> Vec<BufferedFrame> {
        self.first_frame_ms = None;
        std::mem::take(&mut self.frames)
    }
}

#[derive(Debug, Default)]
pub struct ReceiverState {
    per_user: HashMap<String, UserAudioBuffer>,
}

impl ReceiverState {
    pub fn ensure_user(&mut self, user_id: &str) -> &mut UserAudioBuffer {
        self.per_user
            .entry(user_id.to_owned())
            .or_insert_with(|| UserAudioBuffer::new(user_id.to_owned()))
    }

    pub fn track_frame(&mut self, user_id: &str, frame: BufferedFrame) {
        self.ensure_user(user_id).push_frame(frame);
    }

    pub fn users_ready_to_flush<'a>(
        &'a self,
        now_ms: u64,
        config: &ReceiverConfig,
    ) -> Vec<&'a str> {
        self.per_user
            .values()
            .filter(|buf| buf.should_flush(now_ms, config))
            .map(|buf| buf.user_id.as_str())
            .collect()
    }

    pub fn take_user_chunk(&mut self, user_id: &str) -> Option<Vec<BufferedFrame>> {
        let user = self.per_user.get_mut(user_id)?;
        let chunk = user.take_frames();
        if chunk.is_empty() { None } else { Some(chunk) }
    }

    pub fn flush_due_chunks(
        &mut self,
        now_ms: u64,
        config: &ReceiverConfig,
    ) -> Vec<UserChunkCandidate> {
        let user_ids: Vec<String> = self
            .users_ready_to_flush(now_ms, config)
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();

        let mut out = Vec::new();
        for user_id in user_ids {
            if let Some(frames) = self.take_user_chunk(&user_id) {
                out.push(UserChunkCandidate { user_id, frames });
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserChunkCandidate {
    pub user_id: String,
    pub frames: Vec<BufferedFrame>,
}
