use crate::audio::receiver::{BufferedFrame, ReceiverConfig};
use crate::audio::recorder::{RecorderEngine, RecorderError, RecorderOutputChunk};
use crate::audio::songbird_adapter::SsrcTracker;
use crate::infrastructure::storage_fs::{ChunkStorage, ChunkStorageError, LocalChunkStorage, SavedChunk};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedChunk {
    pub user_id: String,
    pub sequence: u64,
    pub start_ms: u64,
    pub saved: SavedChunk,
}

/// Result of a flush operation.  Callers should inspect `failed` —
/// those chunks have been drained from the recorder and could not be
/// persisted.  The caller may retry storage or delay session teardown.
#[derive(Debug)]
pub struct FlushResult {
    pub persisted: Vec<PersistedChunk>,
    pub failed: Vec<RecorderOutputChunk>,
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

    pub fn flush_due(&mut self, now: Instant) -> Result<FlushResult, RecordingSessionError> {
        let chunks = self.recorder.flush_due(now)?;
        Ok(self.persist_chunks(chunks))
    }

    pub fn flush_all(&mut self) -> Result<FlushResult, RecordingSessionError> {
        let chunks = self.recorder.flush_all()?;
        Ok(self.persist_chunks(chunks))
    }

    /// Persist chunks best-effort.  Successfully saved chunks are returned in
    /// `persisted`; chunks whose storage write failed are returned in `failed`
    /// so the caller can decide whether to retry or accept the loss.
    fn persist_chunks(&mut self, chunks: Vec<RecorderOutputChunk>) -> FlushResult {
        let mut persisted = Vec::with_capacity(chunks.len());
        let mut failed = Vec::new();

        for chunk in chunks {
            let saved = self.storage.save_chunk(
                &self.meeting_id,
                &chunk.user_id,
                // Sequence is assigned only after successful persistence to
                // avoid gaps when a save fails.
                self.peek_next_sequence(&chunk.user_id),
                chunk.start_ms,
                &chunk.wav.bytes,
            );
            match saved {
                Ok(saved) => {
                    self.commit_sequence(&chunk.user_id);
                    let seq = self.current_sequence(&chunk.user_id);
                    persisted.push(PersistedChunk {
                        user_id: chunk.user_id,
                        sequence: seq,
                        start_ms: chunk.start_ms,
                        saved,
                    });
                }
                Err(err) => {
                    tracing::warn!(
                        meeting_id = %self.meeting_id,
                        user_id = %chunk.user_id,
                        error = %err,
                        "failed to persist audio chunk — returning to caller for retry"
                    );
                    failed.push(chunk);
                }
            }
        }

        FlushResult { persisted, failed }
    }

    /// Returns the next sequence number without committing it.
    fn peek_next_sequence(&self, user_id: &str) -> u64 {
        self.per_user_seq.get(user_id).copied().unwrap_or(0) + 1
    }

    /// Commits the sequence number (increments the counter).
    fn commit_sequence(&mut self, user_id: &str) {
        let seq = self.per_user_seq.entry(user_id.to_owned()).or_insert(0);
        *seq += 1;
    }

    /// Returns the current (already committed) sequence number for a user.
    fn current_sequence(&self, user_id: &str) -> u64 {
        self.per_user_seq.get(user_id).copied().unwrap_or(0)
    }

    /// Re-key in-memory audio buffers and sequence counters from `old_id`
    /// to `new_id`. Returns the number of in-memory frames moved.
    pub fn rekey_user(&mut self, old_id: &str, new_id: &str) -> usize {
        let moved = self.recorder.rekey_user(old_id, new_id);
        if let Some(old_seq) = self.per_user_seq.remove(old_id) {
            let new_seq = self.per_user_seq.entry(new_id.to_owned()).or_insert(0);
            *new_seq = (*new_seq).max(old_seq);
        }
        moved
    }
}

impl RecordingSession<LocalChunkStorage> {
    /// Persist the SSRC-to-user mapping as a JSON file in the audio directory.
    pub fn persist_ssrc_mapping(&self, tracker: &SsrcTracker) {
        if tracker.all_mappings().is_empty() {
            return;
        }
        let path = self.storage.workspace.ssrc_mapping_path();
        match serde_json::to_vec_pretty(tracker) {
            Ok(json) => {
                if let Err(err) = std::fs::write(&path, &json) {
                    tracing::warn!(
                        meeting_id = %self.meeting_id,
                        path = %path.display(),
                        error = %err,
                        "failed to persist SSRC mapping"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    meeting_id = %self.meeting_id,
                    error = %err,
                    "failed to serialize SSRC mapping"
                );
            }
        }
    }
}
