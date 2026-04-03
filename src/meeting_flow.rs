use crate::asr::WhisperClient;
use crate::recording_session::{PersistedChunk, RecordingSession, RecordingSessionError};
use crate::recovery::RecoveryCandidate;
use crate::recovery_runner::{RecoveryEffect, RecoveryRunnerError, run_recovery};
use crate::retention::{
    ArtifactRecord, CleanupCandidate, RetentionPolicy, select_cleanup_candidates,
};
use crate::storage::{MeetingStore, StoreError};
use crate::storage_fs::ChunkStorage;
use crate::summary::ClaudeSummaryClient;
use crate::worker::{
    ProcessMeetingInput, ProcessMeetingOutput, WorkerError, process_meeting_summary,
};
use std::fmt::{Display, Formatter};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingFlowOutput {
    pub recovery_effect: RecoveryEffect,
    pub persisted_chunks: Vec<PersistedChunk>,
    pub summary: ProcessMeetingOutput,
    pub cleanup_candidates: Vec<CleanupCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeetingFlowError {
    Recovery(String),
    Recording(String),
    Summary(String),
    Store(String),
}

impl Display for MeetingFlowError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recovery(err) => write!(f, "recovery error: {err}"),
            Self::Recording(err) => write!(f, "recording error: {err}"),
            Self::Summary(err) => write!(f, "summary error: {err}"),
            Self::Store(err) => write!(f, "store error: {err}"),
        }
    }
}

impl std::error::Error for MeetingFlowError {}

impl From<RecoveryRunnerError> for MeetingFlowError {
    fn from(value: RecoveryRunnerError) -> Self {
        Self::Recovery(value.to_string())
    }
}

impl From<RecordingSessionError> for MeetingFlowError {
    fn from(value: RecordingSessionError) -> Self {
        Self::Recording(value.to_string())
    }
}

impl From<WorkerError> for MeetingFlowError {
    fn from(value: WorkerError) -> Self {
        Self::Summary(value.to_string())
    }
}

impl From<StoreError> for MeetingFlowError {
    fn from(value: StoreError) -> Self {
        Self::Store(value.to_string())
    }
}

pub fn run_meeting_flow<S, C, W, FS>(
    store: &mut S,
    recording_session: &mut RecordingSession<FS>,
    recovery_candidate: &RecoveryCandidate,
    now: Instant,
    whisper: &W,
    claude: &C,
    summary_input: &ProcessMeetingInput,
    retention_records: &[ArtifactRecord],
    now_unix_seconds: u64,
    retention_policy: RetentionPolicy,
) -> Result<MeetingFlowOutput, MeetingFlowError>
where
    S: MeetingStore,
    C: ClaudeSummaryClient,
    W: WhisperClient,
    FS: ChunkStorage,
{
    let recovery_effect = run_recovery(store, recovery_candidate)?;
    let persisted_chunks = recording_session.flush_due(now)?;
    let summary = process_meeting_summary(store, whisper, claude, summary_input)?;
    let cleanup_candidates =
        select_cleanup_candidates(retention_records, now_unix_seconds, retention_policy);

    Ok(MeetingFlowOutput {
        recovery_effect,
        persisted_chunks,
        summary,
        cleanup_candidates,
    })
}
