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

    pub fn take_frames(&mut self) -> (u64, Vec<BufferedFrame>) {
        let start_ms = self.first_frame_ms.unwrap_or(0);
        self.first_frame_ms = None;
        self.first_frame_instant = None;
        (start_ms, std::mem::take(&mut self.frames))
    }

    /// Merge another buffer into this one. Frames are combined and
    /// sorted by timestamp. The earliest timing metadata is kept.
    pub fn merge_from(&mut self, other: UserAudioBuffer) {
        self.frames.extend(other.frames);
        self.frames.sort_by_key(|f| f.timestamp_ms);
        self.first_frame_ms = match (self.first_frame_ms, other.first_frame_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.first_frame_instant = match (self.first_frame_instant, other.first_frame_instant) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
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

    pub fn take_user_chunk(&mut self, user_id: &str) -> Option<UserChunkCandidate> {
        let user = self.per_user.get_mut(user_id)?;
        let (start_ms, frames) = user.take_frames();
        if frames.is_empty() {
            None
        } else {
            Some(UserChunkCandidate {
                user_id: user.user_id.clone(),
                start_ms,
                frames,
            })
        }
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

        user_ids
            .into_iter()
            .filter_map(|user_id| self.take_user_chunk(&user_id))
            .collect()
    }

    pub fn flush_all_chunks(&mut self) -> Vec<UserChunkCandidate> {
        let user_ids: Vec<String> = self.per_user.keys().cloned().collect();
        user_ids
            .into_iter()
            .filter_map(|user_id| self.take_user_chunk(&user_id))
            .collect()
    }

    /// Re-key buffered frames from `old_id` to `new_id`.
    /// If `new_id` already has a buffer, frames are merged in timestamp order.
    /// Returns the number of frames moved.
    pub fn rekey_user(&mut self, old_id: &str, new_id: &str) -> usize {
        let Some(old_buf) = self.per_user.remove(old_id) else {
            return 0;
        };
        let moved = old_buf.frames.len();
        if let Some(existing) = self.per_user.get_mut(new_id) {
            existing.merge_from(old_buf);
        } else {
            let mut buf = old_buf;
            buf.user_id = new_id.to_owned();
            self.per_user.insert(new_id.to_owned(), buf);
        }
        moved
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserChunkCandidate {
    pub user_id: String,
    pub start_ms: u64,
    pub frames: Vec<BufferedFrame>,
}
