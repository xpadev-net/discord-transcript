use crate::audio::wav::{MAX_WAV_CHUNK_PCM_BYTES, pcm_duration_ms};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const MAX_RECEIVER_USERS: usize = 256;
pub const MAX_RECEIVER_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_RECEIVER_BUFFERED_FRAMES_PER_USER: usize = 16_384;
pub const MAX_RECEIVER_BUFFERED_BYTES_PER_USER: usize = MAX_WAV_CHUNK_PCM_BYTES;
pub const MAX_RECEIVER_BUFFERED_BYTES_TOTAL: usize = 512 * 1024 * 1024;
const RECEIVER_SPAN_SAMPLE_RATE: u32 = 48_000;
const MAX_RECEIVER_BUFFERED_SPAN_MS: u64 =
    (MAX_WAV_CHUNK_PCM_BYTES as u64 / 2) * 1_000 / RECEIVER_SPAN_SAMPLE_RATE as u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverConfig {
    pub chunk_duration: Duration,
    pub silence_flush_duration: Duration,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            chunk_duration: Duration::from_secs(60),
            silence_flush_duration: Duration::from_secs(30),
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
    last_frame_instant: Option<Instant>,
    buffered_bytes: usize,
    frame_instants: Vec<Instant>,
    min_start_ms: Option<u64>,
    max_end_ms: Option<u64>,
}

impl UserAudioBuffer {
    pub fn new(user_id: String) -> Self {
        Self {
            user_id,
            frames: Vec::new(),
            first_frame_ms: None,
            first_frame_instant: None,
            last_frame_instant: None,
            buffered_bytes: 0,
            frame_instants: Vec::new(),
            min_start_ms: None,
            max_end_ms: None,
        }
    }

    pub fn push_frame(&mut self, frame: BufferedFrame) {
        let now = Instant::now();
        let frame_end_ms = frame_end_ms(&frame).unwrap_or(u64::MAX);
        self.buffered_bytes = self
            .buffered_bytes
            .saturating_add(frame.pcm_16le_bytes.len());
        self.min_start_ms = Some(
            self.min_start_ms
                .map_or(frame.timestamp_ms, |start| start.min(frame.timestamp_ms)),
        );
        self.max_end_ms = Some(
            self.max_end_ms
                .map_or(frame_end_ms, |end| end.max(frame_end_ms)),
        );
        if self.first_frame_ms.is_none() {
            self.first_frame_ms = Some(frame.timestamp_ms);
            self.first_frame_instant = Some(now);
        }
        self.last_frame_instant = Some(now);
        self.frames.push(frame);
        self.frame_instants.push(now);
    }

    /// Uses monotonic clock (Instant) so NTP adjustments cannot stall or
    /// prematurely trigger flushes. Pass `Instant::now()` in production.
    pub fn should_flush(&self, now: Instant, config: &ReceiverConfig) -> bool {
        let Some(start) = self.first_frame_instant else {
            return false;
        };
        if now.saturating_duration_since(start) >= config.chunk_duration {
            return true;
        }
        self.last_frame_instant.is_some_and(|last| {
            now.saturating_duration_since(last) >= config.silence_flush_duration
        })
    }

    pub fn take_frames(&mut self) -> (u64, Vec<BufferedFrame>) {
        let start_ms = self.first_frame_ms.unwrap_or(0);
        self.first_frame_ms = None;
        self.first_frame_instant = None;
        self.last_frame_instant = None;
        self.buffered_bytes = 0;
        self.frame_instants.clear();
        self.min_start_ms = None;
        self.max_end_ms = None;
        (start_ms, std::mem::take(&mut self.frames))
    }

    /// Merge another buffer into this one. Frames are combined and
    /// sorted by timestamp. The earliest timing metadata is kept.
    pub fn merge_from(&mut self, mut other: UserAudioBuffer) {
        self.buffered_bytes = self.buffered_bytes.saturating_add(other.buffered_bytes);
        let mut timed_frames = std::mem::take(&mut self.frames)
            .into_iter()
            .zip(std::mem::take(&mut self.frame_instants))
            .collect::<Vec<_>>();
        timed_frames.extend(
            std::mem::take(&mut other.frames)
                .into_iter()
                .zip(std::mem::take(&mut other.frame_instants)),
        );
        timed_frames.sort_by_key(|(frame, _)| frame.timestamp_ms);
        let (frames, instants): (Vec<_>, Vec<_>) = timed_frames.into_iter().unzip();
        self.frames = frames;
        self.frame_instants = instants;
        self.refresh_metadata();
    }

    fn can_accept_frame(&self, frame: &BufferedFrame, frame_end_ms: u64) -> bool {
        self.frames.len() < MAX_RECEIVER_BUFFERED_FRAMES_PER_USER
            && self
                .buffered_bytes
                .checked_add(frame.pcm_16le_bytes.len())
                .is_some_and(|bytes| bytes <= MAX_RECEIVER_BUFFERED_BYTES_PER_USER)
            && self.buffered_span_after(frame, frame_end_ms) <= MAX_RECEIVER_BUFFERED_SPAN_MS
    }

    fn buffered_span_after(&self, frame: &BufferedFrame, frame_end_ms: u64) -> u64 {
        let start_ms = self
            .min_start_ms
            .map_or(frame.timestamp_ms, |start| start.min(frame.timestamp_ms));
        let end_ms = self
            .max_end_ms
            .map_or(frame_end_ms, |end| end.max(frame_end_ms));
        end_ms.saturating_sub(start_ms)
    }

    fn refresh_metadata(&mut self) {
        self.first_frame_ms = self.frames.first().map(|frame| frame.timestamp_ms);
        self.first_frame_instant = self.frame_instants.iter().min().copied();
        self.last_frame_instant = self.frame_instants.iter().max().copied();
        self.min_start_ms = self.frames.iter().map(|frame| frame.timestamp_ms).min();
        self.max_end_ms = self
            .frames
            .iter()
            .map(|frame| frame_end_ms(frame).unwrap_or(u64::MAX))
            .max();
    }

    fn drop_oldest_over_limits(&mut self) -> usize {
        let mut dropped_bytes = 0usize;
        let mut drop_count = 0usize;
        let mut retained_bytes = self.buffered_bytes;

        let mut suffix_max_end_ms = vec![0u64; self.frames.len()];
        let mut running_max_end_ms = 0u64;
        for (index, frame) in self.frames.iter().enumerate().rev() {
            running_max_end_ms = running_max_end_ms.max(frame_end_ms(frame).unwrap_or(u64::MAX));
            suffix_max_end_ms[index] = running_max_end_ms;
        }

        for frame in &self.frames {
            let retained_len = self.frames.len().saturating_sub(drop_count);
            let span_ok = retained_len == 0
                || suffix_max_end_ms[drop_count]
                    .checked_sub(self.frames[drop_count].timestamp_ms)
                    .is_some_and(|span_ms| span_ms <= MAX_RECEIVER_BUFFERED_SPAN_MS);
            if retained_len <= MAX_RECEIVER_BUFFERED_FRAMES_PER_USER
                && retained_bytes <= MAX_RECEIVER_BUFFERED_BYTES_PER_USER
                && span_ok
            {
                break;
            }
            let bytes = frame.pcm_16le_bytes.len();
            retained_bytes = retained_bytes.saturating_sub(bytes);
            dropped_bytes = dropped_bytes.saturating_add(bytes);
            drop_count += 1;
        }

        if drop_count > 0 {
            self.frames.drain(0..drop_count);
            self.frame_instants.drain(0..drop_count);
            self.buffered_bytes = retained_bytes;
            self.refresh_metadata();
        }

        dropped_bytes
    }
}

fn frame_end_ms(frame: &BufferedFrame) -> Option<u64> {
    frame.timestamp_ms.checked_add(pcm_duration_ms(
        &frame.pcm_16le_bytes,
        RECEIVER_SPAN_SAMPLE_RATE,
    ))
}

#[derive(Debug, Default)]
pub struct ReceiverState {
    per_user: HashMap<String, UserAudioBuffer>,
    buffered_bytes: usize,
}

impl ReceiverState {
    pub fn ensure_user(&mut self, user_id: &str) -> &mut UserAudioBuffer {
        self.per_user
            .entry(user_id.to_owned())
            .or_insert_with(|| UserAudioBuffer::new(user_id.to_owned()))
    }

    pub fn track_frame(&mut self, user_id: &str, frame: BufferedFrame) {
        let frame_bytes = frame.pcm_16le_bytes.len();
        if frame_bytes > MAX_RECEIVER_FRAME_BYTES {
            tracing::warn!(
                user_id,
                frame_bytes,
                max_bytes = MAX_RECEIVER_FRAME_BYTES,
                "dropping oversized audio frame"
            );
            return;
        }

        let Some(frame_end_ms) = frame_end_ms(&frame) else {
            tracing::warn!(
                user_id,
                timestamp_ms = frame.timestamp_ms,
                frame_bytes,
                "dropping audio frame because receiver frame timestamp overflows"
            );
            return;
        };

        if !self.per_user.contains_key(user_id) && self.per_user.len() >= MAX_RECEIVER_USERS {
            tracing::warn!(
                user_id,
                users = self.per_user.len(),
                max_users = MAX_RECEIVER_USERS,
                "dropping audio frame because receiver user limit is reached"
            );
            return;
        }

        if self
            .buffered_bytes
            .checked_add(frame_bytes)
            .is_none_or(|bytes| bytes > MAX_RECEIVER_BUFFERED_BYTES_TOTAL)
        {
            tracing::warn!(
                user_id,
                buffered_bytes = self.buffered_bytes,
                frame_bytes,
                max_bytes = MAX_RECEIVER_BUFFERED_BYTES_TOTAL,
                "dropping audio frame because receiver global buffer limit is reached"
            );
            return;
        }

        if let Some(user) = self.per_user.get(user_id) {
            if !user.can_accept_frame(&frame, frame_end_ms) {
                tracing::warn!(
                    user_id,
                    frame_bytes,
                    buffered_bytes = user.buffered_bytes,
                    buffered_frames = user.frames.len(),
                    max_bytes = MAX_RECEIVER_BUFFERED_BYTES_PER_USER,
                    max_frames = MAX_RECEIVER_BUFFERED_FRAMES_PER_USER,
                    "dropping audio frame because receiver user buffer limit is reached"
                );
                return;
            }
        } else if frame_end_ms.saturating_sub(frame.timestamp_ms) > MAX_RECEIVER_BUFFERED_SPAN_MS {
            tracing::warn!(
                user_id,
                frame_bytes,
                max_span_ms = MAX_RECEIVER_BUFFERED_SPAN_MS,
                "dropping audio frame because receiver user buffer span limit is reached"
            );
            return;
        }

        let user = self.ensure_user(user_id);
        user.push_frame(frame);
        self.buffered_bytes = self.buffered_bytes.saturating_add(frame_bytes);
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
        let mut user = self.per_user.remove(user_id)?;
        let buffered_bytes = user.buffered_bytes;
        let (start_ms, frames) = user.take_frames();
        self.buffered_bytes = self.buffered_bytes.saturating_sub(buffered_bytes);
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
            let dropped_bytes = existing.drop_oldest_over_limits();
            if dropped_bytes > 0 {
                self.buffered_bytes = self.buffered_bytes.saturating_sub(dropped_bytes);
                tracing::warn!(
                    old_id,
                    new_id,
                    dropped_bytes,
                    max_bytes = MAX_RECEIVER_BUFFERED_BYTES_PER_USER,
                    max_frames = MAX_RECEIVER_BUFFERED_FRAMES_PER_USER,
                    "dropped oldest frames while re-keying receiver buffer"
                );
            }
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
