use crate::audio::receiver::{BufferedFrame, ReceiverConfig};
use crate::audio::recorder::{RecorderEngine, RecorderError, RecorderOutputChunk};
use crate::audio::songbird_adapter::SsrcTracker;
use crate::infrastructure::storage_fs::{
    ChunkStorage, ChunkStorageError, LocalChunkStorage, SavedChunk,
};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::time::Instant;

const MAX_PENDING_FAILED_CHUNK_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedChunk {
    pub meeting_id: String,
    pub user_id: String,
    pub sequence: u64,
    pub start_ms: u64,
    pub saved: SavedChunk,
}

/// Result of a flush operation.  Callers should inspect `failed` —
/// those chunks have been retained by the session for retry, so callers
/// can delay teardown without copying raw audio bytes.
#[derive(Debug)]
pub struct FlushResult {
    pub persisted: Vec<PersistedChunk>,
    pub failed: Vec<FailedChunk>,
    pub newly_failed: usize,
    pub audio_loss: AudioLoss,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AudioLoss {
    pub dropped_chunks: usize,
    pub dropped_bytes: usize,
}

impl AudioLoss {
    pub fn is_empty(self) -> bool {
        self.dropped_chunks == 0 && self.dropped_bytes == 0
    }

    fn record_drop(&mut self, chunks: usize, bytes: usize) {
        self.dropped_chunks = self.dropped_chunks.saturating_add(chunks);
        self.dropped_bytes = self.dropped_bytes.saturating_add(bytes);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedChunk {
    pub user_id: String,
    pub start_ms: u64,
    pub size_bytes: usize,
}

impl FailedChunk {
    fn from_chunk(chunk: &RecorderOutputChunk) -> Self {
        Self {
            user_id: chunk.user_id.clone(),
            start_ms: chunk.start_ms,
            size_bytes: chunk.wav.bytes.len(),
        }
    }
}

#[derive(Debug)]
struct PersistChunksResult {
    persisted: Vec<PersistedChunk>,
    failed_chunks: Vec<RecorderOutputChunk>,
    newly_failed: usize,
}

#[derive(Debug)]
pub struct RecordingSession<S: ChunkStorage> {
    pub meeting_id: String,
    recorder: RecorderEngine,
    storage: S,
    per_user_seq: HashMap<String, u64>,
    pending_failed_chunks: Vec<RecorderOutputChunk>,
    audio_loss: AudioLoss,
    max_pending_failed_chunk_bytes: usize,
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
            pending_failed_chunks: Vec::new(),
            audio_loss: AudioLoss::default(),
            max_pending_failed_chunk_bytes: MAX_PENDING_FAILED_CHUNK_BYTES,
        }
    }

    pub fn ingest_frame(&mut self, user_id: &str, frame: BufferedFrame) {
        self.recorder.ingest_frame(user_id, frame);
    }

    pub fn flush_due(&mut self, now: Instant) -> Result<FlushResult, RecordingSessionError> {
        let chunks = self.recorder.flush_due(now)?;
        Ok(self.persist_chunks_with_pending(chunks, false))
    }

    pub fn flush_all(&mut self) -> Result<FlushResult, RecordingSessionError> {
        let chunks = self.recorder.flush_all()?;
        Ok(self.persist_chunks_with_pending(chunks, true))
    }

    fn persist_chunks_with_pending(
        &mut self,
        chunks: Vec<RecorderOutputChunk>,
        retry_pending_without_new_chunks: bool,
    ) -> FlushResult {
        if chunks.is_empty() && !retry_pending_without_new_chunks {
            // `flush_due` is called on every VoiceTick. Pending chunks remain
            // retained internally, but no-op ticks report no fresh failures so
            // callers do not warn on every tick during a storage outage.
            return FlushResult {
                persisted: vec![],
                failed: vec![],
                newly_failed: 0,
                audio_loss: self.audio_loss,
            };
        }
        let existing_pending_count = self.pending_failed_chunks.len();
        let mut retry_chunks = std::mem::take(&mut self.pending_failed_chunks);
        retry_chunks.extend(chunks);
        let result = self.persist_chunks(retry_chunks, existing_pending_count);
        self.pending_failed_chunks = result.failed_chunks;
        self.enforce_pending_failed_limit();
        FlushResult {
            persisted: result.persisted,
            failed: self.pending_failed_metadata(),
            newly_failed: result.newly_failed,
            audio_loss: self.audio_loss,
        }
    }

    fn pending_failed_metadata(&self) -> Vec<FailedChunk> {
        self.pending_failed_chunks
            .iter()
            .map(FailedChunk::from_chunk)
            .collect()
    }

    fn enforce_pending_failed_limit(&mut self) {
        let mut total_bytes: usize = self
            .pending_failed_chunks
            .iter()
            .map(|chunk| chunk.wav.bytes.len())
            .sum();
        if total_bytes <= self.max_pending_failed_chunk_bytes {
            return;
        }

        let mut drop_count = 0usize;
        let mut drop_bytes = 0usize;
        for chunk in &self.pending_failed_chunks {
            if total_bytes <= self.max_pending_failed_chunk_bytes {
                break;
            }
            let size = chunk.wav.bytes.len();
            total_bytes = total_bytes.saturating_sub(size);
            drop_bytes += size;
            drop_count += 1;
        }

        if drop_count > 0 {
            self.pending_failed_chunks.drain(0..drop_count);
            self.audio_loss.record_drop(drop_count, drop_bytes);
            tracing::warn!(
                meeting_id = %self.meeting_id,
                dropped_chunks = drop_count,
                dropped_bytes = drop_bytes,
                retained_bytes = total_bytes,
                max_bytes = self.max_pending_failed_chunk_bytes,
                "pending failed audio chunk buffer exceeded memory limit; dropped oldest chunks"
            );
        }
    }

    /// Persist chunks best-effort.  Successfully saved chunks are returned in
    /// `persisted`; chunks whose storage write failed are returned in `failed`
    /// so the caller can decide whether to retry or accept the loss.
    fn persist_chunks(
        &mut self,
        chunks: Vec<RecorderOutputChunk>,
        existing_pending_count: usize,
    ) -> PersistChunksResult {
        let mut persisted = Vec::with_capacity(chunks.len());
        let mut failed_chunks = Vec::new();
        let mut newly_failed = 0usize;

        for (index, chunk) in chunks.into_iter().enumerate() {
            let saved = self.storage.save_chunk(
                &self.meeting_id,
                &chunk.user_id,
                // Sequence is assigned only after successful persistence to
                // avoid gaps when a save fails. Downstream audio assembly sorts
                // by start_ms first; sequence is only a filename/tie-breaker.
                self.peek_next_sequence(&chunk.user_id),
                chunk.start_ms,
                &chunk.wav.bytes,
            );
            match saved {
                Ok(saved) => {
                    self.commit_sequence(&chunk.user_id);
                    let seq = self.current_sequence(&chunk.user_id);
                    persisted.push(PersistedChunk {
                        meeting_id: self.meeting_id.clone(),
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
                    if index >= existing_pending_count {
                        newly_failed += 1;
                    }
                    failed_chunks.push(chunk);
                }
            }
        }

        PersistChunksResult {
            persisted,
            failed_chunks,
            newly_failed,
        }
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
        for chunk in &mut self.pending_failed_chunks {
            if chunk.user_id == old_id {
                chunk.user_id = new_id.to_owned();
            }
        }
        if let Some(old_seq) = self.per_user_seq.remove(old_id) {
            let new_seq = self.per_user_seq.entry(new_id.to_owned()).or_insert(0);
            *new_seq = (*new_seq).max(old_seq);
        }
        moved
    }

    #[cfg(test)]
    pub(crate) fn set_pending_failed_chunk_limit_for_tests(&mut self, max_bytes: usize) {
        self.max_pending_failed_chunk_bytes = max_bytes;
    }
}

impl RecordingSession<LocalChunkStorage> {
    /// Persist the SSRC-to-user mapping as a JSON file in the audio directory.
    /// Only mappings for users recorded in this session are included.
    pub fn persist_ssrc_mapping(&self, tracker: &SsrcTracker) {
        let users: HashSet<&str> = self
            .per_user_seq
            .keys()
            .map(String::as_str)
            .chain(
                self.pending_failed_chunks
                    .iter()
                    .map(|chunk| chunk.user_id.as_str()),
            )
            .collect();
        let filtered = tracker.filtered_by_users(users);
        if filtered.all_mappings().is_empty() {
            return;
        }
        let path = self.storage.workspace.ssrc_mapping_path();
        match serde_json::to_vec_pretty(&filtered) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::storage_fs::SavedChunk;
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Debug, Clone)]
    struct FlakyMemoryChunkStorage {
        failures_remaining: Arc<AtomicUsize>,
    }

    impl ChunkStorage for FlakyMemoryChunkStorage {
        fn save_chunk(
            &self,
            _meeting_id: &str,
            user_id: &str,
            sequence: u64,
            start_ms: u64,
            bytes: &[u8],
        ) -> Result<SavedChunk, ChunkStorageError> {
            if self
                .failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    if value > 0 { Some(value - 1) } else { None }
                })
                .is_ok()
            {
                return Err(ChunkStorageError::Io("injected failure".to_owned()));
            }
            Ok(SavedChunk {
                path: PathBuf::from(format!("{user_id}_{sequence}_{start_ms}.wav")),
                size_bytes: bytes.len(),
            })
        }
    }

    fn test_session(failures: Arc<AtomicUsize>) -> RecordingSession<FlakyMemoryChunkStorage> {
        RecordingSession::new(
            "meeting-1".to_owned(),
            FlakyMemoryChunkStorage {
                failures_remaining: failures,
            },
            ReceiverConfig {
                chunk_duration: std::time::Duration::from_secs(20),
                silence_flush_duration: std::time::Duration::from_secs(30),
            },
            48_000,
        )
    }

    fn ingest_test_frame(
        session: &mut RecordingSession<FlakyMemoryChunkStorage>,
        timestamp_ms: u64,
    ) {
        session.ingest_frame(
            "u1",
            BufferedFrame {
                timestamp_ms,
                pcm_16le_bytes: vec![0, 0, 1, 0],
            },
        );
    }

    #[test]
    fn dropped_pending_failed_chunks_are_reported_after_later_successful_flush() {
        let failures = Arc::new(AtomicUsize::new(1));
        let mut session = test_session(Arc::clone(&failures));
        session.set_pending_failed_chunk_limit_for_tests(1);

        ingest_test_frame(&mut session, 1_000);
        let dropped = session.flush_all().expect("flush should not fail hard");

        assert_eq!(dropped.newly_failed, 1);
        assert!(dropped.failed.is_empty());
        assert_eq!(dropped.audio_loss.dropped_chunks, 1);
        assert!(dropped.audio_loss.dropped_bytes > 0);

        failures.store(0, Ordering::SeqCst);
        ingest_test_frame(&mut session, 2_000);
        let recovered = session.flush_all().expect("later flush should succeed");

        assert_eq!(recovered.persisted.len(), 1);
        assert!(recovered.failed.is_empty());
        assert_eq!(recovered.audio_loss.dropped_chunks, 1);
        assert_eq!(
            recovered.audio_loss.dropped_bytes,
            dropped.audio_loss.dropped_bytes
        );
    }
}
