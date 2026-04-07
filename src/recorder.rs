use crate::audio::{AudioError, WavChunk, build_wav_chunk};
use crate::receiver::{BufferedFrame, ReceiverConfig, ReceiverState, UserChunkCandidate};
use std::fmt::{Display, Formatter};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderOutputChunk {
    pub user_id: String,
    pub start_ms: u64,
    pub wav: WavChunk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecorderError {
    Audio(String),
}

impl Display for RecorderError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Audio(err) => write!(f, "audio error: {err}"),
        }
    }
}

impl std::error::Error for RecorderError {}

impl From<AudioError> for RecorderError {
    fn from(value: AudioError) -> Self {
        Self::Audio(value.to_string())
    }
}

#[derive(Debug)]
pub struct RecorderEngine {
    receiver: ReceiverState,
    receiver_config: ReceiverConfig,
    sample_rate: u32,
}

impl RecorderEngine {
    pub fn new(receiver_config: ReceiverConfig, sample_rate: u32) -> Self {
        Self {
            receiver: ReceiverState::default(),
            receiver_config,
            sample_rate,
        }
    }

    pub fn ingest_frame(&mut self, user_id: &str, frame: BufferedFrame) {
        self.receiver.track_frame(user_id, frame);
    }

    pub fn flush_due(&mut self, now: Instant) -> Result<Vec<RecorderOutputChunk>, RecorderError> {
        let due = self.receiver.flush_due_chunks(now, &self.receiver_config);
        Ok(self.build_chunks_best_effort(due))
    }

    pub fn flush_all(&mut self) -> Result<Vec<RecorderOutputChunk>, RecorderError> {
        let all = self.receiver.flush_all_chunks();
        Ok(self.build_chunks_best_effort(all))
    }

    /// Build WAV chunks best-effort: individual user errors are logged and
    /// skipped so that one user's bad audio does not discard all other users'
    /// chunks in the same tick.
    fn build_chunks_best_effort(
        &self,
        candidates: Vec<UserChunkCandidate>,
    ) -> Vec<RecorderOutputChunk> {
        let mut out = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            match self.build_chunk(candidate) {
                Ok(chunk) => out.push(chunk),
                Err(err) => {
                    tracing::warn!(error = %err, "skipping audio chunk due to build error");
                }
            }
        }
        out
    }

    fn build_chunk(
        &self,
        candidate: UserChunkCandidate,
    ) -> Result<RecorderOutputChunk, RecorderError> {
        let wav = build_wav_chunk(&candidate.frames, self.sample_rate)?;
        Ok(RecorderOutputChunk {
            user_id: candidate.user_id,
            start_ms: candidate.start_ms,
            wav,
        })
    }
}
