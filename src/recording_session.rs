use crate::receiver::{BufferedFrame, ReceiverConfig};
use crate::recorder::{RecorderEngine, RecorderError};
use crate::storage_fs::{ChunkStorage, ChunkStorageError, SavedChunk};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedChunk {
    pub user_id: String,
    pub sequence: u64,
    pub saved: SavedChunk,
}

#[derive(Debug)]
pub struct RecordingSession<S: ChunkStorage> {
    pub meeting_id: String,
    recorder: RecorderEngine,
    storage: S,
    per_user_seq: HashMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingSessionError {
    Recorder(String),
    Storage(String),
}

impl Display for RecordingSessionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recorder(err) => write!(f, "recorder error: {err}"),
            Self::Storage(err) => write!(f, "storage error: {err}"),
        }
    }
}

impl std::error::Error for RecordingSessionError {}

impl From<RecorderError> for RecordingSessionError {
    fn from(value: RecorderError) -> Self {
        Self::Recorder(value.to_string())
    }
}

impl From<ChunkStorageError> for RecordingSessionError {
    fn from(value: ChunkStorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl<S: ChunkStorage> RecordingSession<S> {
    pub fn new(
        meeting_id: String,
        storage: S,
        receiver_config: ReceiverConfig,
        sample_rate: u32,
    ) -> Self {
        Self {
            meeting_id,
            recorder: RecorderEngine::new(receiver_config, sample_rate),
            storage,
            per_user_seq: HashMap::new(),
        }
    }

    pub fn ingest_frame(&mut self, user_id: &str, frame: BufferedFrame) {
        self.recorder.ingest_frame(user_id, frame);
    }

    pub fn flush_due(&mut self, now_ms: u64) -> Result<Vec<PersistedChunk>, RecordingSessionError> {
        let chunks = self.recorder.flush_due(now_ms)?;
        let mut persisted = Vec::with_capacity(chunks.len());

        for chunk in chunks {
            let sequence = self.next_sequence(&chunk.user_id);
            let saved = self.storage.save_chunk(
                &self.meeting_id,
                &chunk.user_id,
                sequence,
                &chunk.wav.bytes,
            )?;
            persisted.push(PersistedChunk {
                user_id: chunk.user_id,
                sequence,
                saved,
            });
        }

        Ok(persisted)
    }

    fn next_sequence(&mut self, user_id: &str) -> u64 {
        let seq = self.per_user_seq.entry(user_id.to_owned()).or_insert(0);
        *seq += 1;
        *seq
    }
}
