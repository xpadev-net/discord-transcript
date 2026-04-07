use crate::application::recovery_runner::{RecoveryEffect, RecoveryRunnerError, run_recovery};
use crate::application::summary::ClaudeSummaryClient;
use crate::application::worker::{
    ProcessMeetingInput, ProcessMeetingOutput, WorkerError, process_meeting_summary,
};
use crate::audio::recording_session::{PersistedChunk, RecordingSession, RecordingSessionError};
use crate::domain::recovery::RecoveryCandidate;
use crate::domain::retention::{
    ArtifactRecord, CleanupCandidate, RetentionPolicy, select_cleanup_candidates,
};
use crate::infrastructure::asr::WhisperClient;
use crate::infrastructure::storage::{MeetingStore, StoreError};
use crate::infrastructure::storage_fs::ChunkStorage;
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

#[derive(Debug, Clone, Copy)]
pub struct MeetingFlowInput<'a, W, C> {
    recovery_candidate: &'a RecoveryCandidate,
    now: Instant,
    whisper: &'a W,
    claude: &'a C,
    summary_input: &'a ProcessMeetingInput,
    retention_records: &'a [ArtifactRecord],
    now_unix_seconds: u64,
    retention_policy: RetentionPolicy,
}

impl<'a, W, C> MeetingFlowInput<'a, W, C> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        recovery_candidate: &'a RecoveryCandidate,
        now: Instant,
        whisper: &'a W,
        claude: &'a C,
        summary_input: &'a ProcessMeetingInput,
        retention_records: &'a [ArtifactRecord],
        now_unix_seconds: u64,
        retention_policy: RetentionPolicy,
    ) -> Self {
        Self {
            recovery_candidate,
            now,
            whisper,
            claude,
            summary_input,
            retention_records,
            now_unix_seconds,
            retention_policy,
        }
    }
}

pub fn run_meeting_flow<S, C, W, FS>(
    store: &mut S,
    recording_session: &mut RecordingSession<FS>,
    input: MeetingFlowInput<'_, W, C>,
) -> Result<MeetingFlowOutput, MeetingFlowError>
where
    S: MeetingStore,
    C: ClaudeSummaryClient,
    W: WhisperClient,
    FS: ChunkStorage,
{
    let recovery_effect = run_recovery(store, input.recovery_candidate)?;
    let flush_result = recording_session.flush_due(input.now)?;
    let summary = process_meeting_summary(store, input.whisper, input.claude, input.summary_input)?;
    let cleanup_candidates = select_cleanup_candidates(
        input.retention_records,
        input.now_unix_seconds,
        input.retention_policy,
    );

    Ok(MeetingFlowOutput {
        recovery_effect,
        persisted_chunks: flush_result.persisted,
        summary,
        cleanup_candidates,
    })
}
