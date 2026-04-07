use crate::domain::{JobStatus, JobType, MeetingStatus};
use crate::queue::{Job, JobQueue, QueueError};
use crate::storage::{MeetingStore, StoreError};
use crate::summary::{
    ClaudeSummaryClient, SummaryError, SummaryRequest, build_summary_prompt, run_transcription,
    write_transcript_files,
};
use crate::workspace::MeetingWorkspacePaths;
use crate::{asr::WhisperClient, posting::split_discord_message};
use std::fmt::{Display, Formatter};
use tracing::{error, info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeetingInput {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub title: Option<String>,
    pub audio_path: String,
    pub language: Option<String>,
    pub workspace: MeetingWorkspacePaths,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeetingOutput {
    pub meeting_id: String,
    pub markdown: String,
    pub chunks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerError {
    Queue(String),
    Store(String),
    Summary(String),
    /// A summary job with the same ID was already present in the queue.
    /// The caller should treat this as "a claimable job already exists" and
    /// proceed to claim it rather than treating it as a fatal error.
    AlreadyExists,
}

impl Display for WorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queue(err) => write!(f, "queue error: {err}"),
            Self::Store(err) => write!(f, "store error: {err}"),
            Self::Summary(err) => write!(f, "summary error: {err}"),
            Self::AlreadyExists => write!(f, "summary job already exists in queue"),
        }
    }
}

impl std::error::Error for WorkerError {}

impl From<StoreError> for WorkerError {
    fn from(value: StoreError) -> Self {
        Self::Store(value.to_string())
    }
}

impl From<QueueError> for WorkerError {
    fn from(value: QueueError) -> Self {
        Self::Queue(value.to_string())
    }
}

impl From<SummaryError> for WorkerError {
    fn from(value: SummaryError) -> Self {
        Self::Summary(value.to_string())
    }
}

pub fn process_meeting_summary<S: MeetingStore, W: WhisperClient, C: ClaudeSummaryClient>(
    store: &mut S,
    whisper: &W,
    claude: &C,
    input: &ProcessMeetingInput,
) -> Result<ProcessMeetingOutput, WorkerError> {
    info!(meeting_id = %input.meeting_id, "summary pipeline started");

    let request = SummaryRequest {
        meeting_id: input.meeting_id.clone(),
        guild_id: input.guild_id.clone(),
        voice_channel_id: input.voice_channel_id.clone(),
        title: input.title.clone(),
        audio_path: input.audio_path.clone(),
        language: input.language.clone(),
        workspace: input.workspace.clone(),
    };

    store.set_meeting_status(
        &input.meeting_id,
        MeetingStatus::Transcribing,
        Some(MeetingStatus::Stopping),
    )?;
    let transcription = match run_transcription(whisper, &request) {
        Ok(value) => value,
        Err(err) => {
            error!(meeting_id = %input.meeting_id, error = %err, "transcription failed");
            // Revert to Stopping so the next retry attempt's CAS guard succeeds.
            let _ = store.set_meeting_status(
                &input.meeting_id,
                MeetingStatus::Stopping,
                Some(MeetingStatus::Transcribing),
            );
            return Err(WorkerError::from(err));
        }
    };

    store.set_meeting_status(
        &input.meeting_id,
        MeetingStatus::Summarizing,
        Some(MeetingStatus::Transcribing),
    )?;
    let manifest = match write_transcript_files(&request, &transcription) {
        Ok(value) => value,
        Err(err) => {
            error!(meeting_id = %input.meeting_id, error = %err, "transcript materialization failed");
            let _ = store.set_meeting_status(
                &input.meeting_id,
                MeetingStatus::Stopping,
                Some(MeetingStatus::Summarizing),
            );
            return Err(WorkerError::from(err));
        }
    };
    let prompt = build_summary_prompt(&request, &manifest);
    let markdown = match claude.summarize(&prompt, Some(request.workspace.root())) {
        Ok(value) => value,
        Err(err) => {
            error!(meeting_id = %input.meeting_id, error = %err, "summarization failed");
            // Revert to Stopping so the next retry attempt starts from a consistent state.
            let _ = store.set_meeting_status(
                &input.meeting_id,
                MeetingStatus::Stopping,
                Some(MeetingStatus::Summarizing),
            );
            return Err(WorkerError::from(err));
        }
    };

    let chunks = split_discord_message(&markdown, crate::posting::DISCORD_MESSAGE_LIMIT);
    info!(
        meeting_id = %input.meeting_id,
        chunks = chunks.len(),
        "summary pipeline completed"
    );

    Ok(ProcessMeetingOutput {
        meeting_id: input.meeting_id.clone(),
        markdown,
        chunks,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessJobResult {
    pub job_id: String,
    pub output: ProcessMeetingOutput,
}

pub fn process_next_summary_job<S, Q, W, C>(
    store: &mut S,
    queue: &mut Q,
    whisper: &W,
    claude: &C,
    max_retries: u32,
    audio_base_dir: &str,
    language: Option<String>,
) -> Result<Option<ProcessJobResult>, WorkerError>
where
    S: MeetingStore,
    Q: JobQueue,
    W: WhisperClient,
    C: ClaudeSummaryClient,
{
    let Some(job) = queue.claim_next(JobType::Summarize)? else {
        return Ok(None);
    };
    info!(job_id = %job.id, meeting_id = %job.meeting_id, "claimed summary job");

    let meeting = store
        .get_meeting(&job.meeting_id)
        .map_err(WorkerError::from)?
        .ok_or_else(|| {
            WorkerError::Store(format!("meeting not found for summary: {}", job.meeting_id))
        })?;
    let layout = crate::workspace::MeetingWorkspaceLayout::new(audio_base_dir);
    let workspace = layout.for_meeting(
        &meeting.guild_id,
        &meeting.voice_channel_id,
        &job.meeting_id,
    );
    workspace.ensure_base_dirs().map_err(|err| {
        WorkerError::from(SummaryError::SummaryEngine(format!(
            "failed to prepare workspace: {err}"
        )))
    })?;
    let primary_mixdown = workspace.mixdown_path();
    let legacy_mixdown = layout
        .legacy_meeting_dir(&job.meeting_id)
        .join("mixdown.wav");
    let audio_path = if primary_mixdown.exists() {
        primary_mixdown
    } else {
        legacy_mixdown
    };

    let input = ProcessMeetingInput {
        meeting_id: job.meeting_id.clone(),
        guild_id: meeting.guild_id.clone(),
        voice_channel_id: meeting.voice_channel_id.clone(),
        title: meeting.title.clone(),
        audio_path: audio_path.to_string_lossy().to_string(),
        language,
        workspace,
    };

    match process_meeting_summary(store, whisper, claude, &input) {
        Ok(output) => {
            // Set meeting status first: if this fails the job stays Running
            // and can be retried. The reverse order (mark_done first) would
            // leave the meeting stuck in Summarizing with no way to recover.
            store.set_meeting_status(
                &job.meeting_id,
                MeetingStatus::Posted,
                Some(MeetingStatus::Summarizing),
            )?;
            queue.mark_done(&job.id)?;
            info!(job_id = %job.id, "summary job marked done");
            Ok(Some(ProcessJobResult {
                job_id: job.id,
                output,
            }))
        }
        Err(err) => {
            let status = queue.retry(&job.id, err.to_string(), max_retries)?;
            if status == JobStatus::Failed {
                store.set_meeting_status(&job.meeting_id, MeetingStatus::Failed, None)?;
                store.set_error_message(&job.meeting_id, Some(err.to_string()))?;
                warn!(
                    job_id = %job.id,
                    meeting_id = %job.meeting_id,
                    "summary job exhausted retries"
                );
            } else {
                info!(
                    job_id = %job.id,
                    meeting_id = %job.meeting_id,
                    "summary job queued for retry"
                );
            }
            Err(err)
        }
    }
}

pub fn enqueue_summary_job<Q: JobQueue>(
    queue: &mut Q,
    job_id: &str,
    meeting_id: &str,
) -> Result<(), WorkerError> {
    match queue.enqueue(Job {
        id: job_id.to_owned(),
        meeting_id: meeting_id.to_owned(),
        job_type: JobType::Summarize,
        status: JobStatus::Queued,
        retry_count: 0,
        error_message: None,
    }) {
        Ok(()) => {}
        Err(QueueError::AlreadyExists { .. }) => return Err(WorkerError::AlreadyExists),
        Err(err) => return Err(WorkerError::Queue(err.to_string())),
    }
    info!(job_id = %job_id, meeting_id = %meeting_id, "summary job enqueued");
    Ok(())
}
