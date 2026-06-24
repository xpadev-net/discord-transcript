use discord_transcript::application::summary::StubClaudeSummaryClient;
use discord_transcript::application::worker::{
    SummaryJobOptions, SummaryNotificationReceipt, SummaryStatusNotification,
    SummaryUrlNotification, complete_summary_job_after_notification, enqueue_summary_job,
    process_next_summary_job, record_summary_completion_usage_observe_only,
};
use discord_transcript::domain::{JobStatus, JobType, MeetingStatus};
use discord_transcript::domain::usage::UsageMetric;
use discord_transcript::infrastructure::asr::{
    StubWhisperClient, WhisperClient, WhisperInferenceRequest, WhisperParseError,
    WhisperTranscriptionResult, parse_whisper_response,
};
use discord_transcript::infrastructure::queue::{InMemoryJobQueue, JobQueue};
use discord_transcript::infrastructure::sql::{
    ADMIN_CANCEL_JOB_SQL, ADMIN_RETRY_JOB_SQL, CLAIM_JOB_BY_ID_SQL, CLAIM_JOB_SQL,
    RECOVERY_READY_SUMMARY_JOBS_SQL, RETRY_JOB_SQL,
};
use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlJobQueue, sql_row_from_strings,
};
use discord_transcript::infrastructure::storage::{
    EffectiveMeetingSettings, InMemoryMeetingStore, MeetingStore, StoredMeeting, UsageEventStore,
};
use chrono::{DateTime, Duration, Utc};
use std::cell::RefCell;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

fn stopping_meeting(id: &str) -> StoredMeeting {
    meeting_with_status(id, MeetingStatus::Stopping)
}

fn meeting_with_status(id: &str, status: MeetingStatus) -> StoredMeeting {
    StoredMeeting {
        id: id.to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
    voice_channel_name: None,
        report_channel_id: "tc1".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status,
        stop_reason: None,
        error_message: None,
        started_at: None,
        stopped_at: None,
        duration_seconds: None,
    }
}

fn effective_settings(summary_enabled: bool) -> EffectiveMeetingSettings {
    EffectiveMeetingSettings {
        whisper_language: Some("snapshot-ja".to_owned()),
        whisper_vad: false,
        whisper_beam_size: 8,
        whisper_suppress_non_speech: false,
        whisper_prompt: Some("snapshot prompt".to_owned()),
        whisper_temperature: 0.25,
        whisper_resample_to_16k: false,
        auto_stop_grace_seconds: 120,
        retention_raw_audio_ttl_days: 14,
        retention_transcript_ttl_days: 60,
        retention_summary_ttl_days: None,
        summary_enabled,
        summary_template_id: None,
        domain_knowledge_version_id: None,
    }
}

struct RecordingWhisperClient {
    mocked_response_json: String,
    requests: RefCell<Vec<WhisperInferenceRequest>>,
}

impl RecordingWhisperClient {
    fn new() -> Self {
        Self {
            mocked_response_json: r#"{
              "text":"ok",
              "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
            }"#
            .to_owned(),
            requests: RefCell::new(Vec::new()),
        }
    }
}

impl WhisperClient for RecordingWhisperClient {
    fn infer(
        &self,
        request: &WhisperInferenceRequest,
    ) -> Result<WhisperTranscriptionResult, WhisperParseError> {
        self.requests.borrow_mut().push(request.clone());
        parse_whisper_response(&self.mocked_response_json)
    }
}

struct RecordingSummaryClient {
    calls: RefCell<usize>,
}

impl discord_transcript::application::summary::ClaudeSummaryClient for RecordingSummaryClient {
    fn summarize(
        &self,
        _prompt: &str,
        _workdir: Option<&Path>,
    ) -> Result<String, discord_transcript::application::summary::SummaryError> {
        *self.calls.borrow_mut() += 1;
        Ok("## Summary\ndone".to_owned())
    }
}

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_dir_all(&self.path)
            && err.kind() != ErrorKind::NotFound
        {
            eprintln!(
                "failed to remove temp test directory {}: {err}",
                self.path.display()
            );
        }
    }
}

fn unique_temp_dir(test_name: &str) -> TempDirGuard {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    TempDirGuard {
        path: std::env::temp_dir().join(format!("discord_transcript_job_queue_{test_name}_{nanos}")),
    }
}

fn write_dummy_chunk(base: &Path, meeting_id: &str) {
    use discord_transcript::audio::build_wav_bytes_raw;
    let meeting_dir =
        discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout::new(base)
            .for_meeting("g1", "vc1", meeting_id)
            .audio_dir();
    std::fs::create_dir_all(&meeting_dir).expect("meeting dir should be created");
    let wav = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).expect("wav should build");
    std::fs::write(meeting_dir.join("u1_1_0.wav"), wav).expect("wav should write");
}

fn write_dummy_legacy_chunk(base: &Path, meeting_id: &str) {
    use discord_transcript::audio::build_wav_bytes_raw;
    let meeting_dir =
        discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout::new(base)
            .legacy_meeting_dir(meeting_id);
    std::fs::create_dir_all(&meeting_dir).expect("meeting dir should be created");
    let wav = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).expect("wav should build");
    std::fs::write(meeting_dir.join("u1_1_0.wav"), wav).expect("wav should write");
}

fn write_empty_wav_chunk(base: &Path, meeting_id: &str) {
    use discord_transcript::audio::build_wav_bytes_raw;
    let meeting_dir =
        discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout::new(base)
            .for_meeting("g1", "vc1", meeting_id)
            .audio_dir();
    std::fs::create_dir_all(&meeting_dir).expect("meeting dir should be created");
    let wav = build_wav_bytes_raw(&[], 1_000, 1, 16).expect("wav header should build");
    std::fs::write(meeting_dir.join("u1_1_0.wav"), wav).expect("wav should write");
}

fn write_temp_wav_chunk(base: &Path, meeting_id: &str) {
    use discord_transcript::audio::build_wav_bytes_raw;
    let meeting_dir =
        discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout::new(base)
            .for_meeting("g1", "vc1", meeting_id)
            .audio_dir();
    std::fs::create_dir_all(&meeting_dir).expect("meeting dir should be created");
    let wav = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).expect("wav should build");
    std::fs::write(meeting_dir.join("u1_1_0.wav.tmp"), wav).expect("tmp wav should write");
}

fn write_nonempty_pcm_chunk(base: &Path, meeting_id: &str) {
    let meeting_dir =
        discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout::new(base)
            .for_meeting("g1", "vc1", meeting_id)
            .audio_dir();
    std::fs::create_dir_all(&meeting_dir).expect("meeting dir should be created");
    std::fs::write(meeting_dir.join("u1_2_1000.pcm"), vec![1_u8; 128]).expect("pcm should write");
}

fn fixed_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-16T00:00:00.000Z")
        .expect("fixed test timestamp should parse")
        .with_timezone(&Utc)
}

#[test]
fn in_memory_queue_claim_done_and_retry_flow() {
    let now = fixed_now();
    let mut queue = InMemoryJobQueue::new_at(now);
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let claimed = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(claimed.status, JobStatus::Running);
    assert_eq!(claimed.leased_until, Some(now + Duration::seconds(90)));

    queue
        .mark_done(&claimed)
        .expect("mark done should succeed");
    assert_eq!(queue.get("j1").expect("job exists").status, JobStatus::Done);

    enqueue_summary_job(&mut queue, "j2", "m2").expect("enqueue should succeed");
    let claimed2 = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(claimed2.id, "j2");
    let status = queue
        .retry(&claimed2, "failed once".to_owned(), 2)
        .expect("retry should succeed");
    assert_eq!(status, JobStatus::Queued);
    let retried = queue.get(&claimed2.id).expect("retried job should exist");
    assert_eq!(retried.next_run_at, Some(now + Duration::seconds(30)));
    assert!(
        queue
            .claim_next(JobType::Summarize)
            .expect("future retry claim should succeed")
            .is_none(),
        "future next_run_at should prevent a tight retry loop"
    );
    queue.advance_now(Duration::seconds(29));
    assert!(
        queue
            .claim_next(JobType::Summarize)
            .expect("early retry claim should succeed")
            .is_none(),
        "retry should stay unavailable before next_run_at"
    );
    queue.advance_now(Duration::seconds(1));
    let retried_claim = queue
        .claim_next(JobType::Summarize)
        .expect("due retry claim should succeed")
        .expect("retry should be due exactly at next_run_at");
    assert_eq!(retried_claim.id, "j2");
}

#[test]
fn in_memory_queue_skips_future_next_run_at() {
    let mut queue = InMemoryJobQueue::new();
    queue
        .enqueue(discord_transcript::infrastructure::queue::Job {
            id: "j-future".to_owned(),
            meeting_id: "m1".to_owned(),
            job_type: JobType::Summarize,
            status: JobStatus::Queued,
            retry_count: 0,
            error_message: None,
            claim_token: None,
            leased_until: None,
            next_run_at: Some(Utc::now() + Duration::minutes(5)),
        })
        .expect("enqueue should succeed");

    assert!(
        queue
            .claim_next(JobType::Summarize)
            .expect("claim should succeed")
            .is_none()
    );
    assert!(
        queue
            .claim_by_id("j-future")
            .expect("claim by id should succeed")
            .is_none()
    );
}

#[test]
fn in_memory_queue_failed_and_canceled_jobs_are_terminal_for_claims() {
    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j-failed", "m1").expect("enqueue should succeed");
    let failed = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should be claimed");
    let failed_status = queue
        .retry(&failed, "still failing".to_owned(), 0)
        .expect("retry exhaustion should persist");
    assert_eq!(failed_status, JobStatus::Failed);
    assert_eq!(
        queue.get(&failed.id).expect("job should exist").next_run_at,
        None
    );

    enqueue_summary_job(&mut queue, "j-canceled", "m2").expect("enqueue should succeed");
    queue
        .cancel("j-canceled")
        .expect("queued job should cancel");
    assert_eq!(
        queue.get("j-canceled").expect("job should exist").status,
        JobStatus::Canceled
    );

    assert!(
        queue
            .claim_next(JobType::Summarize)
            .expect("claim should succeed")
            .is_none(),
        "terminal failed/canceled jobs must not be claimed"
    );
}

#[test]
fn in_memory_queue_rejects_stale_claim_token() {
    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j-stale", "m1").expect("enqueue should succeed");

    let mut stale = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should be claimed");
    stale.claim_token = Some("stale-token".to_owned());

    let err = queue
        .mark_done(&stale)
        .expect_err("stale claim token must not finish a running job");
    assert!(matches!(
        err,
        discord_transcript::infrastructure::queue::QueueError::InvalidState { .. }
    ));
    assert_eq!(
        queue.get("j-stale").expect("job exists").status,
        JobStatus::Running
    );
}

#[test]
fn in_memory_queue_rejects_expired_lease_and_recovers_stale_running() {
    let now = fixed_now();
    let mut queue = InMemoryJobQueue::new_at(now);
    enqueue_summary_job(&mut queue, "j-expired", "m1").expect("enqueue should succeed");

    let claimed = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should be claimed");
    queue.advance_now(Duration::seconds(89));
    queue
        .heartbeat(&claimed)
        .expect("heartbeat before lease expiry should extend ownership");
    assert_eq!(
        queue.get("j-expired").and_then(|job| job.leased_until),
        Some(now + Duration::seconds(179))
    );

    queue.set_now(now + Duration::seconds(179));
    queue
        .heartbeat(&claimed)
        .expect_err("heartbeat at lease expiry must fail closed");
    queue
        .mark_done(&claimed)
        .expect_err("expired owner must not finish a job");
    queue
        .retry(&claimed, "expired".to_owned(), 2)
        .expect_err("expired owner must not retry a job");
    assert_eq!(
        queue.get("j-expired").expect("job exists").status,
        JobStatus::Running
    );

    let recovered = queue.recover_stale_running(JobType::Summarize, 25);
    assert_eq!(recovered, vec!["m1".to_owned()]);
    let recovered_job = queue.get("j-expired").expect("job exists");
    assert_eq!(recovered_job.status, JobStatus::Queued);
    assert_eq!(recovered_job.claim_token, None);
    assert_eq!(recovered_job.leased_until, None);
    let reclaimed = queue
        .claim_next(JobType::Summarize)
        .expect("reclaim should succeed")
        .expect("recovered job should be claimable");
    assert_eq!(reclaimed.id, "j-expired");
    assert_ne!(reclaimed.claim_token, claimed.claim_token);
}

#[test]
fn worker_job_processing_waits_for_notification_before_completion() {
    let base = unique_temp_dir("worker_success");
    write_dummy_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));

    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"ok",
          "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "## Summary\ndone".to_owned(),
    };

    let result = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 2,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: Some("ja".to_owned()),
            resample_to_16k: false,
        },
    )
    .expect("worker should succeed")
    .expect("job result should exist");
    assert_eq!(result.job_id, "j1");
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Running
    );
    assert_eq!(
        store.get("m1").expect("meeting should exist").status,
        MeetingStatus::Summarizing
    );
    assert_eq!(
        std::fs::read_to_string(
            base.path()
                .join("workspaces/g1/vc1/m1/summary/summary.md")
        )
        .expect("generated summary should be durable"),
        "## Summary\ndone"
    );
    let usage = store
        .list_recent_usage_events(None, Some("g1"), 10)
        .expect("usage should list");
    assert!(
        !usage
            .iter()
            .any(|event| event.metric == UsageMetric::SummaryRuns),
        "summary run usage must wait for notification completion"
    );
    assert!(
        usage
            .iter()
            .any(|event| event.metric == UsageMetric::AsrSeconds
                && event.job_id.as_deref() == Some("j1"))
    );

    let receipt = SummaryNotificationReceipt::new(
        result.output.chunks.len(),
        SummaryUrlNotification::NotConfigured,
        SummaryStatusNotification::Updated,
    )
    .expect("notification receipt should be valid");
    let completed =
        complete_summary_job_after_notification(&mut store, &mut queue, &result.job, receipt)
            .expect("completion should succeed after notification");

    assert!(completed);
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Done
    );
    assert_eq!(
        store.get("m1").expect("meeting should exist").status,
        MeetingStatus::Posted
    );
    record_summary_completion_usage_observe_only(
        &mut store,
        &result.job.meeting_id,
        &result.job.id,
        result.output.chunks.len(),
    );
    let usage = store
        .list_recent_usage_events(None, Some("g1"), 10)
        .expect("usage should list");
    assert!(
        usage
            .iter()
            .any(|event| event.metric == UsageMetric::SummaryRuns
                && event.quantity == 1
                && event.job_id.as_deref() == Some("j1"))
    );
}

#[test]
fn summary_completion_receipt_requires_post_url_and_status_outcomes() {
    let zero_chunks = SummaryNotificationReceipt::new(
        0,
        SummaryUrlNotification::NotConfigured,
        SummaryStatusNotification::Updated,
    )
    .expect_err("zero posted chunks must not complete a summary job");
    assert!(
        zero_chunks
            .to_string()
            .contains("at least one successful Discord post chunk"),
        "unexpected error: {zero_chunks}"
    );

    let missing_url = SummaryNotificationReceipt::new(
        1,
        SummaryUrlNotification::NotAttempted,
        SummaryStatusNotification::Updated,
    )
    .expect_err("URL notification outcome must be explicit");
    assert!(
        missing_url
            .to_string()
            .contains("meeting URL notification outcome"),
        "unexpected error: {missing_url}"
    );

    let missing_status = SummaryNotificationReceipt::new(
        1,
        SummaryUrlNotification::NotConfigured,
        SummaryStatusNotification::NotAttempted,
    )
    .expect_err("status notification outcome must be explicit");
    assert!(
        missing_status
            .to_string()
            .contains("status message update outcome"),
        "unexpected error: {missing_status}"
    );
}

#[test]
fn worker_job_processing_uses_snapshot_language_for_asr() {
    let base = unique_temp_dir("worker_snapshot_language");
    write_dummy_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));
    store
        .upsert_effective_meeting_settings("m1", effective_settings(true))
        .expect("snapshot should be stored");

    let whisper = RecordingWhisperClient::new();
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "## Summary\ndone".to_owned(),
    };

    process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 2,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: Some("option-en".to_owned()),
            resample_to_16k: true,
        },
    )
    .expect("worker should succeed")
    .expect("job result should exist");

    let requests = whisper.requests.borrow();
    assert!(!requests.is_empty(), "ASR should be invoked");
    assert!(
        requests
            .iter()
            .all(|request| request.language.as_deref() == Some("snapshot-ja")),
        "ASR language should come from the meeting snapshot: {requests:?}"
    );
}

#[test]
fn worker_job_processing_does_not_run_disabled_summary_snapshot() {
    let base = unique_temp_dir("worker_summary_disabled");
    write_dummy_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));
    store
        .upsert_effective_meeting_settings("m1", effective_settings(false))
        .expect("snapshot should be stored");

    let whisper = RecordingWhisperClient::new();
    let claude = RecordingSummaryClient {
        calls: RefCell::new(0),
    };

    let result = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 2,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: Some("option-en".to_owned()),
            resample_to_16k: true,
        },
    )
    .expect("disabled summary job should be consumed without work");

    assert!(result.is_none());
    assert!(whisper.requests.borrow().is_empty(), "ASR should not run");
    assert_eq!(*claude.calls.borrow(), 0, "summary should not run");
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Done
    );
    assert_eq!(
        store.get("m1").expect("meeting should exist").status,
        MeetingStatus::Posted
    );
    let usage = store
        .list_recent_usage_events(None, Some("g1"), 10)
        .expect("usage should list");
    assert!(
        !usage
            .iter()
            .any(|event| event.metric == UsageMetric::SummaryRuns),
        "disabled summary jobs must not record summary run usage"
    );
}

#[test]
fn worker_job_processing_marks_disabled_summary_done_when_meeting_already_posted() {
    let base = unique_temp_dir("worker_summary_disabled_posted");
    write_dummy_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(meeting_with_status("m1", MeetingStatus::Posted));
    store
        .upsert_effective_meeting_settings("m1", effective_settings(false))
        .expect("snapshot should be stored");

    let whisper = RecordingWhisperClient::new();
    let claude = RecordingSummaryClient {
        calls: RefCell::new(0),
    };

    let result = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 2,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: Some("option-en".to_owned()),
            resample_to_16k: true,
        },
    )
    .expect("disabled summary job should be consumed without work");

    assert!(result.is_none());
    assert!(whisper.requests.borrow().is_empty(), "ASR should not run");
    assert_eq!(*claude.calls.borrow(), 0, "summary should not run");
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Done
    );
    assert_eq!(
        store.get("m1").expect("meeting should exist").status,
        MeetingStatus::Posted
    );
}

#[test]
fn worker_job_processing_marks_posted_meeting_job_done_without_rerunning_summary() {
    let base = unique_temp_dir("worker_already_posted");
    write_dummy_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(meeting_with_status("m1", MeetingStatus::Posted));

    let whisper = RecordingWhisperClient::new();
    let claude = RecordingSummaryClient {
        calls: RefCell::new(0),
    };

    let result = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 2,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: Some("option-en".to_owned()),
            resample_to_16k: true,
        },
    )
    .expect("posted meeting job should be consumed without work");

    assert!(result.is_none());
    assert!(whisper.requests.borrow().is_empty(), "ASR should not run");
    assert_eq!(*claude.calls.borrow(), 0, "summary should not run");
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Done
    );
    let usage = store
        .list_recent_usage_events(None, Some("g1"), 10)
        .expect("usage should list");
    assert!(
        usage
            .iter()
            .any(|event| event.metric == UsageMetric::SummaryRuns
                && event.job_id.as_deref() == Some("j1"))
    );
}

#[test]
fn worker_job_processing_rejects_disabled_summary_for_non_terminal_pipeline_status() {
    let base = unique_temp_dir("worker_summary_disabled_transcribing");
    write_dummy_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(meeting_with_status("m1", MeetingStatus::Transcribing));
    store
        .upsert_effective_meeting_settings("m1", effective_settings(false))
        .expect("snapshot should be stored");

    let whisper = RecordingWhisperClient::new();
    let claude = RecordingSummaryClient {
        calls: RefCell::new(0),
    };

    let err = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 0,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: Some("option-en".to_owned()),
            resample_to_16k: true,
        },
    )
    .expect_err("disabled summary should not be suppressed from transcribing");

    assert!(
        err.to_string()
            .contains("cannot suppress disabled summary job"),
        "unexpected error: {err}"
    );
    assert!(whisper.requests.borrow().is_empty(), "ASR should not run");
    assert_eq!(*claude.calls.borrow(), 0, "summary should not run");
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Failed
    );
    assert_eq!(
        store.get("m1").expect("meeting should exist").status,
        MeetingStatus::Failed
    );
}

#[test]
fn worker_job_processing_requeues_when_generated_summary_persistence_fails() {
    let base = unique_temp_dir("worker_summary_persist_failure");
    write_dummy_chunk(base.path(), "m1");
    let summary_output_path =
        discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout::new(base.path())
            .for_meeting("g1", "vc1", "m1")
            .summary_dir()
            .join("summary.md");
    std::fs::create_dir_all(&summary_output_path)
        .expect("directory at summary output path should force write failure");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));

    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"ok",
          "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "## Summary\ndone".to_owned(),
    };

    let err = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 2,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: None,
            resample_to_16k: false,
        },
    )
    .expect_err("summary persistence failure should retry");

    assert!(
        err.to_string()
            .contains("failed to persist generated summary"),
        "unexpected error: {err}"
    );
    let job = queue.get("j1").expect("job exists");
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.retry_count, 1);
    let saved = store.get("m1").expect("meeting exists");
    assert_eq!(saved.status, MeetingStatus::Stopping);
}

#[test]
fn worker_job_processing_marks_failed_after_retries_exhausted() {
    let base = unique_temp_dir("worker_failure");
    write_dummy_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));

    let whisper = StubWhisperClient {
        mocked_response_json: "{invalid_json".to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "ignored".to_owned(),
    };

    let result = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 0,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: None,
            resample_to_16k: false,
        },
    );
    assert!(result.is_err(), "should fail with invalid JSON");
    let job = queue.get("j1").expect("job exists");
    assert_eq!(job.status, JobStatus::Failed);
    let saved = store.get("m1").expect("meeting exists");
    assert_eq!(saved.status, MeetingStatus::Failed);
}

#[test]
fn worker_job_processing_rejects_empty_chunks() {
    let base = unique_temp_dir("worker_empty_chunk");
    write_empty_wav_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));

    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"ok",
          "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "ignored".to_owned(),
    };

    let result = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 0,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: None,
            resample_to_16k: false,
        },
    );
    let err = result.expect_err("should fail when only empty chunks exist");
    assert!(
        err.to_string().contains("no non-empty audio chunks found"),
        "unexpected error: {err}"
    );
    let job = queue.get("j1").expect("job exists");
    assert_eq!(job.status, JobStatus::Failed);
    let saved = store.get("m1").expect("meeting exists");
    assert_eq!(saved.status, MeetingStatus::Failed);
}

#[test]
fn worker_job_processing_ignores_temporary_wav_chunks() {
    let base = unique_temp_dir("worker_tmp_chunk");
    write_temp_wav_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));

    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"ok",
          "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "ignored".to_owned(),
    };

    let err = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 0,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: None,
            resample_to_16k: false,
        },
    )
    .expect_err("temporary chunks must not be processed as complete audio");

    assert!(
        err.to_string().contains("no non-empty audio chunks found"),
        "unexpected error: {err}"
    );
    let job = queue.get("j1").expect("job exists");
    assert_eq!(job.status, JobStatus::Failed);
    let saved = store.get("m1").expect("meeting exists");
    assert_eq!(saved.status, MeetingStatus::Failed);
}

#[test]
fn worker_job_processing_rejects_pcm_only_chunks() {
    let base = unique_temp_dir("worker_pcm_only_chunk");
    write_empty_wav_chunk(base.path(), "m1");
    write_nonempty_pcm_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));

    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"ok",
          "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "ignored".to_owned(),
    };

    let result = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 0,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: None,
            resample_to_16k: false,
        },
    );
    let err = result.expect_err("should fail when only pcm chunks are non-empty");
    assert!(
        err.to_string().contains("no non-empty audio chunks found"),
        "unexpected error: {err}"
    );
    let job = queue.get("j1").expect("job exists");
    assert_eq!(job.status, JobStatus::Failed);
    let saved = store.get("m1").expect("meeting exists");
    assert_eq!(saved.status, MeetingStatus::Failed);
}

#[test]
fn worker_job_processing_falls_back_to_legacy_when_workspace_chunks_are_empty() {
    let base = unique_temp_dir("worker_legacy_fallback");
    write_empty_wav_chunk(base.path(), "m1");
    write_dummy_legacy_chunk(base.path(), "m1");

    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(stopping_meeting("m1"));

    let whisper = StubWhisperClient {
        mocked_response_json: r#"{
          "text":"ok",
          "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
        }"#
        .to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "## Summary\ndone".to_owned(),
    };

    let result = process_next_summary_job(
        &mut store,
        &mut queue,
        &whisper,
        &claude,
        &SummaryJobOptions {
            max_retries: 2,
            audio_base_dir: base.path().to_string_lossy().to_string(),
            language: Some("ja".to_owned()),
            resample_to_16k: false,
        },
    )
    .expect("worker should succeed")
    .expect("job result should exist");
    assert_eq!(result.job_id, "j1");
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Running
    );

    let receipt = SummaryNotificationReceipt::new(
        result.output.chunks.len(),
        SummaryUrlNotification::NotConfigured,
        SummaryStatusNotification::Updated,
    )
    .expect("notification receipt should be valid");
    let completed =
        complete_summary_job_after_notification(&mut store, &mut queue, &result.job, receipt)
            .expect("legacy fallback completion should succeed after notification");

    assert!(completed);
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Done
    );
    assert_eq!(
        store.get("m1").expect("meeting should exist").status,
        MeetingStatus::Posted
    );
}

#[test]
fn sql_job_queue_done_job_rejects_retry() {
    let mut executor = FakeSqlExecutor::default();
    let claim_key = format!("{}|*", CLAIM_JOB_SQL);
    executor.query_rows_result.insert(
        claim_key,
        vec![vec![
            Some("j1".to_owned()),
            Some("m1".to_owned()),
            Some("summarize".to_owned()),
            Some("running".to_owned()),
            Some("0".to_owned()),
            None,
            Some("token-1".to_owned()),
            Some("2026-06-08T01:03:00.000Z".to_owned()),
            None,
        ]],
    );
    let retry_key = format!(
        "{}|{}",
        RETRY_JOB_SQL, "j1\u{1f}failed once\u{1f}2\u{1f}token-1"
    );
    executor.query_rows_result.insert(retry_key, Vec::new());

    let mut queue = SqlJobQueue::new(executor);
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let claimed = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(claimed.id, "j1");
    assert_eq!(claimed.status, JobStatus::Running);

    queue
        .mark_done(&claimed)
        .expect("mark done should succeed");
    let err = queue
        .retry(&claimed, "failed once".to_owned(), 2)
        .expect_err("done jobs should not be retryable by SQL");
    assert!(matches!(
        err,
        discord_transcript::infrastructure::queue::QueueError::InvalidState { .. }
    ));
}

#[test]
fn sql_job_queue_running_job_retry_returns_queued() {
    let mut executor = FakeSqlExecutor::default();
    let retry_key = format!(
        "{}|{}",
        RETRY_JOB_SQL, "j1\u{1f}failed once\u{1f}2\u{1f}token-1"
    );
    executor
        .query_rows_result
        .insert(retry_key, vec![sql_row_from_strings(vec!["queued".to_owned()])]);

    let mut queue = SqlJobQueue::new(executor);
    let claimed = discord_transcript::infrastructure::queue::Job {
        id: "j1".to_owned(),
        meeting_id: "m1".to_owned(),
        job_type: JobType::Summarize,
        status: JobStatus::Running,
        retry_count: 0,
        error_message: None,
        claim_token: Some("token-1".to_owned()),
        leased_until: Some(fixed_now() + Duration::seconds(90)),
        next_run_at: None,
    };
    let status = queue
        .retry(&claimed, "failed once".to_owned(), 2)
        .expect("running SQL job should retry");

    assert_eq!(status, JobStatus::Queued);
}

#[test]
fn sql_claims_only_due_queued_jobs() {
    assert!(CLAIM_JOB_SQL.contains("status = 'queued'"));
    assert!(CLAIM_JOB_SQL.contains("next_run_at IS NULL OR next_run_at <= NOW()"));
    assert!(CLAIM_JOB_SQL.contains("FOR UPDATE SKIP LOCKED"));
    assert!(CLAIM_JOB_BY_ID_SQL.contains("job_type = $3"));
    assert!(CLAIM_JOB_BY_ID_SQL.contains("status = 'queued'"));
    assert!(CLAIM_JOB_BY_ID_SQL.contains("next_run_at IS NULL OR next_run_at <= NOW()"));
    assert!(!CLAIM_JOB_SQL.contains("'failed'"));
    assert!(!CLAIM_JOB_SQL.contains("'canceled'"));
}

#[test]
fn sql_retry_schedules_backoff_and_dead_letters_on_exhaustion() {
    assert!(RETRY_JOB_SQL.contains("make_interval"));
    assert!(RETRY_JOB_SQL.contains("LEAST(900"));
    assert!(RETRY_JOB_SQL.contains("status = 'running'"));
    assert!(RETRY_JOB_SQL.contains("claim_token = $4"));
    assert!(RETRY_JOB_SQL.contains("leased_until > NOW()"));
    assert!(RETRY_JOB_SQL.contains("dead_lettered_at"));
    assert!(RETRY_JOB_SQL.contains("finished_at"));
    assert!(RETRY_JOB_SQL.contains("leased_until = NULL"));
}

#[test]
fn sql_running_job_mutations_require_claim_token() {
    assert!(CLAIM_JOB_SQL.contains("claim_token = $2"));
    assert!(CLAIM_JOB_BY_ID_SQL.contains("claim_token = $2"));
    assert!(discord_transcript::infrastructure::sql::MARK_JOB_DONE_SQL.contains(
        "claim_token = $2"
    ));
    assert!(discord_transcript::infrastructure::sql::MARK_JOB_FAILED_SQL.contains(
        "claim_token = $3"
    ));
    assert!(discord_transcript::infrastructure::sql::HEARTBEAT_RUNNING_JOB_SQL.contains(
        "claim_token = $2"
    ));
    assert!(discord_transcript::infrastructure::sql::HEARTBEAT_RUNNING_JOB_SQL.contains(
        "leased_until > NOW()"
    ));
    assert!(discord_transcript::infrastructure::sql::MARK_JOB_DONE_SQL.contains(
        "leased_until > NOW()"
    ));
    assert!(discord_transcript::infrastructure::sql::MARK_JOB_FAILED_SQL.contains(
        "leased_until > NOW()"
    ));
}

#[test]
fn sql_ready_summary_poll_recovers_expired_running_and_due_queued_jobs() {
    assert!(RECOVERY_READY_SUMMARY_JOBS_SQL.contains("status='running'"));
    assert!(RECOVERY_READY_SUMMARY_JOBS_SQL.contains("leased_until <= NOW()"));
    assert!(RECOVERY_READY_SUMMARY_JOBS_SQL.contains("claim_token=NULL"));
    assert!(RECOVERY_READY_SUMMARY_JOBS_SQL.contains("status='queued'"));
    assert!(RECOVERY_READY_SUMMARY_JOBS_SQL.contains("next_run_at <= NOW()"));
    assert!(RECOVERY_READY_SUMMARY_JOBS_SQL.contains("LIMIT 25"));
}

#[test]
fn sql_admin_retry_resets_terminal_state_safely() {
    assert!(ADMIN_RETRY_JOB_SQL.contains("j.status IN ('failed', 'canceled')"));
    assert!(ADMIN_RETRY_JOB_SQL.contains("retry_count = 0"));
    assert!(ADMIN_RETRY_JOB_SQL.contains("dead_lettered_at = NULL"));
    assert!(ADMIN_RETRY_JOB_SQL.contains("canceled_at = NULL"));
    assert!(ADMIN_RETRY_JOB_SQL.contains("cancel_reason = NULL"));
    assert!(ADMIN_RETRY_JOB_SQL.contains("m.guild_id = $2"));
}

#[test]
fn sql_admin_cancel_requires_queued_job_and_records_reason() {
    assert!(ADMIN_CANCEL_JOB_SQL.contains("j.status = 'queued'"));
    assert!(ADMIN_CANCEL_JOB_SQL.contains("status = 'canceled'"));
    assert!(ADMIN_CANCEL_JOB_SQL.contains("cancel_reason = $3"));
    assert!(ADMIN_CANCEL_JOB_SQL.contains("next_run_at = NULL"));
    assert!(ADMIN_CANCEL_JOB_SQL.contains("m.guild_id = $2"));
    assert!(!ADMIN_CANCEL_JOB_SQL.contains("j.status IN"));
}
