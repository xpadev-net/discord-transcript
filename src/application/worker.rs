use crate::application::runtime::merge_user_chunks_to_mixdown;
use crate::application::summary::{
    ClaudeSummaryClient, SpeakerAudioInput, SummaryError, SummaryRequest, TranscriptionOutput,
    build_summary_prompt, correct_transcript, run_transcription, write_transcript_files,
};
use crate::audio::meeting_audio::build_speaker_audio_inputs;
use crate::domain::{JobStatus, JobType, MeetingStatus};
use crate::infrastructure::asr::WhisperClient;
use crate::infrastructure::queue::{Job, JobQueue, QueueError};
use crate::infrastructure::storage::{MeetingStore, StoreError};
use crate::infrastructure::workspace::{MeetingWorkspaceLayout, MeetingWorkspacePaths};
use crate::interfaces::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use std::fmt::{Display, Formatter};
use std::path::Path;
use tracing::{error, info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeetingInput {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub title: Option<String>,
    pub audio_path: String,
    pub speaker_audio: Vec<SpeakerAudioInput>,
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
        speaker_audio: input.speaker_audio.clone(),
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

    // Apply LLM-based error correction to the transcript before summarization.
    let transcription = match correct_transcript(
        claude,
        &transcription.transcript_for_summary,
        request.language.as_deref(),
    ) {
        Ok(corrected) => TranscriptionOutput {
            transcript_for_summary: corrected,
            ..transcription
        },
        Err(err) => {
            warn!(meeting_id = %input.meeting_id, error = %err, "transcript correction failed, using original");
            transcription
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

    let chunks = split_discord_message(&markdown, DISCORD_MESSAGE_LIMIT);
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

fn has_nonempty_audio_chunk(meeting_dir: &Path) -> Result<bool, String> {
    let entries = match std::fs::read_dir(meeting_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(format!(
                "failed to read meeting dir {}: {err}",
                meeting_dir.display()
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read dir entry: {err}"))?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase());
        let is_candidate = matches!(ext.as_deref(), Some("wav"))
            && path.file_stem().and_then(|stem| stem.to_str()) != Some("mixdown");
        if !is_candidate {
            continue;
        }
        let size = entry
            .metadata()
            .map_err(|err| format!("failed to read metadata {}: {err}", path.display()))?
            .len();
        if size > 44 {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, Clone)]
pub struct SummaryJobOptions {
    pub max_retries: u32,
    pub audio_base_dir: String,
    pub language: Option<String>,
    pub resample_to_16k: bool,
}

pub fn process_next_summary_job<S, Q, W, C>(
    store: &mut S,
    queue: &mut Q,
    whisper: &W,
    claude: &C,
    options: &SummaryJobOptions,
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

    let result = (|| {
        let meeting = store
            .get_meeting(&job.meeting_id)
            .map_err(WorkerError::from)?
            .ok_or_else(|| {
                WorkerError::Store(format!("meeting not found for summary: {}", job.meeting_id))
            })?;
        let layout = MeetingWorkspaceLayout::new(&options.audio_base_dir);
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
        let legacy_dir = layout.legacy_meeting_dir(&job.meeting_id);
        let primary_dir = workspace.audio_dir();
        let primary_has_nonempty =
            has_nonempty_audio_chunk(&primary_dir).map_err(WorkerError::Summary)?;
        let meeting_dir = if primary_has_nonempty {
            primary_dir.clone()
        } else {
            let legacy_has_nonempty =
                has_nonempty_audio_chunk(&legacy_dir).map_err(WorkerError::Summary)?;
            if legacy_has_nonempty {
                let expected_mixdown_path = legacy_dir.join("mixdown.wav");
                warn!(
                    meeting_id = %job.meeting_id,
                    path = %expected_mixdown_path.display(),
                    "workspace audio dir missing non-empty chunks; falling back to legacy mixdown path"
                );
                legacy_dir.clone()
            } else {
                return Err(WorkerError::Summary(format!(
                    "no non-empty audio chunks found for meeting {} in {} or {}",
                    job.meeting_id,
                    primary_dir.display(),
                    legacy_dir.display()
                )));
            }
        };

        let mixdown_path = merge_user_chunks_to_mixdown(&meeting_dir, options.resample_to_16k)
            .map_err(WorkerError::Summary)?;
        let input = ProcessMeetingInput {
            meeting_id: job.meeting_id.clone(),
            guild_id: meeting.guild_id.clone(),
            voice_channel_id: meeting.voice_channel_id.clone(),
            title: meeting.title.clone(),
            audio_path: mixdown_path,
            speaker_audio: build_speaker_audio_inputs(&meeting_dir, options.resample_to_16k)
                .map_err(WorkerError::Summary)?,
            language: options.language.clone(),
            workspace,
        };
        process_meeting_summary(store, whisper, claude, &input)
    })();
    match result {
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
            let status = queue.retry(&job.id, err.to_string(), options.max_retries)?;
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
