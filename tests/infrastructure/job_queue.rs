use discord_transcript::application::summary::StubClaudeSummaryClient;
use discord_transcript::application::worker::{enqueue_summary_job, process_next_summary_job};
use discord_transcript::domain::{JobStatus, JobType, MeetingStatus};
use discord_transcript::infrastructure::asr::StubWhisperClient;
use discord_transcript::infrastructure::queue::{InMemoryJobQueue, JobQueue};
use discord_transcript::infrastructure::sql::{CLAIM_JOB_SQL, RETRY_JOB_SQL};
use discord_transcript::infrastructure::sql_store::{FakeSqlExecutor, SqlJobQueue};
use discord_transcript::infrastructure::storage::{InMemoryMeetingStore, StoredMeeting};
use std::path::{Path, PathBuf};

fn stopping_meeting(id: &str) -> StoredMeeting {
    StoredMeeting {
        id: id.to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        report_channel_id: "tc1".to_owned(),
        status_message_channel_id: None,
        status_message_id: None,
        started_by_user_id: "u1".to_owned(),
        title: None,
        status: MeetingStatus::Stopping,
        stop_reason: None,
        error_message: None,
    }
}

fn unique_temp_dir(test_name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("discord_transcript_job_queue_{test_name}_{nanos}"))
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

#[test]
fn in_memory_queue_claim_done_and_retry_flow() {
    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let claimed = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(claimed.status, JobStatus::Running);

    queue
        .mark_done(&claimed.id)
        .expect("mark done should succeed");
    assert_eq!(queue.get("j1").expect("job exists").status, JobStatus::Done);

    enqueue_summary_job(&mut queue, "j2", "m2").expect("enqueue should succeed");
    let claimed2 = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(claimed2.id, "j2");
    let status = queue
        .retry(&claimed2.id, "failed once".to_owned(), 2)
        .expect("retry should succeed");
    assert_eq!(status, JobStatus::Queued);
}

#[test]
fn worker_job_processing_marks_done_on_success() {
    let base = unique_temp_dir("worker_success");
    write_dummy_chunk(&base, "m1");

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
        2,
        base.to_str().unwrap(),
        Some("ja".to_owned()),
    )
    .expect("worker should succeed")
    .expect("job result should exist");
    assert_eq!(result.job_id, "j1");
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Done
    );

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn worker_job_processing_marks_failed_after_retries_exhausted() {
    let base = unique_temp_dir("worker_failure");
    write_dummy_chunk(&base, "m1");

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
        0,
        base.to_str().unwrap(),
        None,
    );
    assert!(result.is_err(), "should fail with invalid JSON");
    let job = queue.get("j1").expect("job exists");
    assert_eq!(job.status, JobStatus::Failed);
    let saved = store.get("m1").expect("meeting exists");
    assert_eq!(saved.status, MeetingStatus::Failed);

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn sql_job_queue_claim_done_and_retry_flow() {
    let mut executor = FakeSqlExecutor::default();
    let claim_key = format!("{}|{}", CLAIM_JOB_SQL, "summarize");
    executor.query_rows_result.insert(
        claim_key,
        vec![vec![
            "j1".to_owned(),
            "m1".to_owned(),
            "summarize".to_owned(),
            "running".to_owned(),
            "0".to_owned(),
            String::new(),
        ]],
    );
    let retry_key = format!("{}|{}", RETRY_JOB_SQL, "j1\u{1f}failed once\u{1f}2");
    executor
        .query_rows_result
        .insert(retry_key, vec![vec!["queued".to_owned()]]);

    let mut queue = SqlJobQueue::new(executor);
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let claimed = queue
        .claim_next(JobType::Summarize)
        .expect("claim should succeed")
        .expect("job should exist");
    assert_eq!(claimed.id, "j1");
    assert_eq!(claimed.status, JobStatus::Running);

    queue
        .mark_done(&claimed.id)
        .expect("mark done should succeed");
    let status = queue
        .retry(&claimed.id, "failed once".to_owned(), 2)
        .expect("retry should succeed");
    assert_eq!(status, JobStatus::Queued);
}
