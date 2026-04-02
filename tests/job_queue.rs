use discord_transcript::asr::StubWhisperClient;
use discord_transcript::domain::{JobStatus, JobType, MeetingStatus};
use discord_transcript::queue::{InMemoryJobQueue, JobQueue};
use discord_transcript::sql::{CLAIM_JOB_SQL, RETRY_JOB_SQL};
use discord_transcript::sql_store::{FakeSqlExecutor, SqlJobQueue};
use discord_transcript::storage::{InMemoryMeetingStore, StoredMeeting};
use discord_transcript::summary::StubClaudeSummaryClient;
use discord_transcript::worker::{enqueue_summary_job, process_next_summary_job};

fn recording_meeting(id: &str) -> StoredMeeting {
    StoredMeeting {
        id: id.to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
        report_channel_id: "tc1".to_owned(),
        started_by_user_id: "u1".to_owned(),
        status: MeetingStatus::Recording,
        stop_reason: None,
        error_message: None,
    }
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
    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(recording_meeting("m1"));

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

    let result = process_next_summary_job(&mut store, &mut queue, &whisper, &claude, 2, "/tmp/chunks")
        .expect("worker should succeed")
        .expect("job result should exist");
    assert_eq!(result.job_id, "j1");
    assert_eq!(
        queue.get("j1").expect("job should exist").status,
        JobStatus::Done
    );
}

#[test]
fn worker_job_processing_marks_failed_after_retries_exhausted() {
    let mut queue = InMemoryJobQueue::new();
    enqueue_summary_job(&mut queue, "j1", "m1").expect("enqueue should succeed");

    let mut store = InMemoryMeetingStore::new();
    store.insert(recording_meeting("m1"));

    let whisper = StubWhisperClient {
        mocked_response_json: "{invalid_json".to_owned(),
    };
    let claude = StubClaudeSummaryClient {
        mocked_markdown: "ignored".to_owned(),
    };

    let result =
        process_next_summary_job(&mut store, &mut queue, &whisper, &claude, 0, "/tmp/chunks");
    assert!(result.is_err(), "should fail with invalid JSON");
    let job = queue.get("j1").expect("job exists");
    assert_eq!(job.status, JobStatus::Failed);
    let saved = store.get("m1").expect("meeting exists");
    assert_eq!(saved.status, MeetingStatus::Failed);
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
