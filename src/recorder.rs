use crate::audio::{AudioError, WavChunk, build_wav_chunk};
use crate::receiver::{BufferedFrame, ReceiverConfig, ReceiverState, UserChunkCandidate};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderOutputChunk {
    pub user_id: String,
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

    pub fn flush_due(&mut self, now_ms: u64) -> Result<Vec<RecorderOutputChunk>, RecorderError> {
        let due = self
            .receiver
            .flush_due_chunks(now_ms, &self.receiver_config);
        due.into_iter()
            .map(|candidate| self.build_chunk(candidate))
            .collect()
    }

    pub fn flush_all(&mut self) -> Result<Vec<RecorderOutputChunk>, RecorderError> {
        let all = self.receiver.flush_all_chunks();
        all.into_iter()
            .map(|candidate| self.build_chunk(candidate))
            .collect()
    }

    fn build_chunk(
        &self,
        candidate: UserChunkCandidate,
    ) -> Result<RecorderOutputChunk, RecorderError> {
        let wav = build_wav_chunk(&candidate.frames, self.sample_rate)?;
        Ok(RecorderOutputChunk {
            user_id: candidate.user_id,
            wav,
        })
    }
}
