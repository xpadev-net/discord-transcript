use std::collections::HashMap;
use std::time::{Duration, Instant};

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

#[derive(Debug)]
pub struct UserAudioBuffer {
    pub user_id: String,
    pub frames: Vec<BufferedFrame>,
    /// Wall-clock timestamp of the first frame (for metadata).
    pub first_frame_ms: Option<u64>,
    /// Monotonic instant when the first frame arrived (for flush timing).
    first_frame_instant: Option<Instant>,
}

impl UserAudioBuffer {
    pub fn new(user_id: String) -> Self {
        Self {
            user_id,
            frames: Vec::new(),
            first_frame_ms: None,
            first_frame_instant: None,
        }
    }

    pub fn push_frame(&mut self, frame: BufferedFrame) {
        if self.first_frame_ms.is_none() {
            self.first_frame_ms = Some(frame.timestamp_ms);
            self.first_frame_instant = Some(Instant::now());
        }
        self.frames.push(frame);
    }

    /// Uses monotonic clock (Instant) so NTP adjustments cannot stall or
    /// prematurely trigger flushes. Pass `Instant::now()` in production.
    pub fn should_flush(&self, now: Instant, config: &ReceiverConfig) -> bool {
        let Some(start) = self.first_frame_instant else {
            return false;
        };
        now.saturating_duration_since(start) >= config.chunk_duration
    }

    pub fn take_frames(&mut self) -> Vec<BufferedFrame> {
        self.first_frame_ms = None;
        self.first_frame_instant = None;
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
        now: Instant,
        config: &ReceiverConfig,
    ) -> Vec<&'a str> {
        self.per_user
            .values()
            .filter(|buf| buf.should_flush(now, config))
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
        now: Instant,
        config: &ReceiverConfig,
    ) -> Vec<UserChunkCandidate> {
        let user_ids: Vec<String> = self
            .users_ready_to_flush(now, config)
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

    pub fn flush_all_chunks(&mut self) -> Vec<UserChunkCandidate> {
        let user_ids: Vec<String> = self.per_user.keys().cloned().collect();
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
