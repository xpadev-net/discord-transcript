use discord_transcript::infrastructure::sql::{
    ADMIN_RETRY_JOB_INPUT_SQL, ADMIN_RETRY_JOB_SQL,
};
use discord_transcript::domain::{JobStatus, MeetingStatus};
use discord_transcript::infrastructure::queue::JobQueue;
use discord_transcript::infrastructure::sql_store::{PgSqlExecutor, SqlExecutor, SqlMeetingStore};
use discord_transcript::infrastructure::sql_store::SqlJobQueue;
use discord_transcript::infrastructure::storage::MeetingStore;
use std::sync::{Arc, Barrier};

#[test]
fn postgres_manual_summary_retry_restores_consistent_state_atomically() {
    let database_url = match std::env::var("DISCORD_TRANSCRIPT_TEST_DATABASE_URL") {
        Ok(database_url) => database_url,
        Err(_) if std::env::var_os("CI").is_some() => {
            panic!("DISCORD_TRANSCRIPT_TEST_DATABASE_URL is required in CI")
        }
        Err(_) => {
            eprintln!("skipping real Postgres manual retry contract test; set DISCORD_TRANSCRIPT_TEST_DATABASE_URL");
            return;
        }
    };

    let schema = format!("dt_manual_retry_{}", uuid::Uuid::new_v4().simple());
    eprintln!("running real Postgres manual retry contract test in isolated schema {schema}");
    let mut migrator = PgSqlExecutor::connect(&database_url).expect("postgres connection");
    migrator
        .run_migration(&format!("CREATE SCHEMA {schema}; SET search_path TO {schema}"))
        .expect("test schema should be created and selected");
    SqlMeetingStore::new(migrator)
        .apply_pending_migrations()
        .expect("schema migrations should apply");

    let result = run_manual_retry_contract(&database_url, &schema);

    let mut cleanup = PgSqlExecutor::connect(&database_url).expect("cleanup connection");
    cleanup
        .run_migration(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .expect("test schema should be dropped");

    result.expect("Postgres manual retry contract should pass");
}

fn run_manual_retry_contract(database_url: &str, schema: &str) -> Result<(), String> {
    let mut executor = executor_for(database_url, schema)?;
    insert_failed_summary(&mut executor, "m-valid", "job-valid", "g1")?;
    let input = executor.query_rows(
        ADMIN_RETRY_JOB_INPUT_SQL,
        &["job-valid".to_owned(), "g1".to_owned()],
    )?;
    if input.len() != 1
        || input[0].first().and_then(|value| value.as_deref()) != Some("m-valid")
        || input[0].get(2).and_then(|value| value.as_deref()) != Some("true")
    {
        return Err(format!("eligible retry input should be returned, got {input:?}"));
    }

    let retried = retry(&mut executor, "job-valid", "g1", true)?;
    if retried.len() != 1
        || retried[0].first().and_then(|value| value.as_deref()) != Some("job-valid")
        || retried[0].get(4).and_then(|value| value.as_deref()) != Some("queued")
        || retried[0].len() != 15
    {
        return Err(format!("retry should return the queued job projection, got {retried:?}"));
    }
    assert_meeting_job_state(&mut executor, "m-valid", "job-valid", "stopping", "queued")?;

    let mut queue = SqlJobQueue::new(executor_for(database_url, schema)?);
    let claimed = queue
        .claim_by_id("job-valid")
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "retried summary job should be claimable by the normal queue".to_owned())?;
    if claimed.status != JobStatus::Running {
        return Err(format!("claimed retry should be running, got {:?}", claimed.status));
    }
    SqlMeetingStore::new(executor_for(database_url, schema)?)
        .set_meeting_status(
            "m-valid",
            MeetingStatus::Transcribing,
            Some(MeetingStatus::Stopping),
        )
        .map_err(|err| err.to_string())?;
    let resumed = executor.query_rows(
        "SELECT m.status, j.status FROM meetings m JOIN jobs j ON j.meeting_id=m.id WHERE m.id=$1 AND j.id=$2",
        &["m-valid".to_owned(), "job-valid".to_owned()],
    )?;
    if resumed.first().and_then(|row| row.first()).and_then(|value| value.as_deref())
        != Some("transcribing")
        || resumed
            .first()
            .and_then(|row| row.get(1))
            .and_then(|value| value.as_deref())
            != Some("running")
    {
        return Err(format!("retried job should resume through normal claim/CAS, got {resumed:?}"));
    }

    insert_failed_summary(&mut executor, "m-empty", "job-empty", "g1")?;
    executor.execute(
        "UPDATE meetings SET retention_raw_cleaned_at=NOW() WHERE id=$1",
        &["m-empty".to_owned()],
    )?;
    if !retry(&mut executor, "job-empty", "g1", false)?.is_empty() {
        return Err("retry must not queue a job when retained inputs are unavailable".to_owned());
    }
    assert_meeting_job_state(&mut executor, "m-empty", "job-empty", "failed", "failed")?;
    insert_failed_summary(&mut executor, "m-other-guild", "job-other-guild", "g2")?;
    insert_failed_summary(&mut executor, "m-posted", "job-posted", "g1")?;
    executor.execute(
        "UPDATE meetings SET status='posted' WHERE id=$1",
        &["m-posted".to_owned()],
    )?;
    insert_failed_summary(&mut executor, "m-done", "job-done", "g1")?;
    executor.execute(
        "UPDATE jobs SET status='done' WHERE id=$1",
        &["job-done".to_owned()],
    )?;
    for (job_id, guild_id) in [
        ("job-other-guild", "g1"),
        ("job-posted", "g1"),
        ("job-done", "g1"),
    ] {
        if !retry(&mut executor, job_id, guild_id, true)?.is_empty() {
            return Err(format!("non-target retry {job_id} should remain unchanged"));
        }
    }
    assert_meeting_job_state(&mut executor, "m-other-guild", "job-other-guild", "failed", "failed")?;
    assert_meeting_job_state(&mut executor, "m-posted", "job-posted", "posted", "failed")?;
    assert_meeting_job_state(&mut executor, "m-done", "job-done", "failed", "done")?;
    insert_failed_summary(&mut executor, "m-running", "job-running", "g1")?;
    executor.execute(
        "UPDATE jobs SET status='running' WHERE id=$1",
        &["job-running".to_owned()],
    )?;
    if !retry(&mut executor, "job-running", "g1", true)?.is_empty() {
        return Err("running job should not be eligible for manual retry".to_owned());
    }
    assert_meeting_job_state(
        &mut executor,
        "m-running",
        "job-running",
        "failed",
        "running",
    )?;

    insert_failed_summary(&mut executor, "m-concurrent", "job-concurrent", "g1")?;
    assert_one_concurrent_retry_wins(database_url, schema)?;
    assert_meeting_job_state(&mut executor, "m-concurrent", "job-concurrent", "stopping", "queued")?;

    insert_failed_summary(&mut executor, "m-atomic", "job-atomic", "g1")?;
    executor.run_migration(
        "CREATE FUNCTION reject_manual_retry_job() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.id = 'job-atomic' AND NEW.status = 'queued' THEN RAISE EXCEPTION 'injected retry failure'; END IF; RETURN NEW; END $$",
    )?;
    executor.run_migration(
        "CREATE TRIGGER reject_manual_retry_job BEFORE UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION reject_manual_retry_job()",
    )?;
    if retry(&mut executor, "job-atomic", "g1", true).is_ok() {
        return Err("injected job update failure should abort retry statement".to_owned());
    }
    assert_meeting_job_state(&mut executor, "m-atomic", "job-atomic", "failed", "failed")?;

    Ok(())
}

fn executor_for(database_url: &str, schema: &str) -> Result<PgSqlExecutor, String> {
    let mut executor = PgSqlExecutor::connect(database_url)?;
    executor.run_migration(&format!("SET search_path TO {schema}"))?;
    Ok(executor)
}

fn insert_failed_summary(
    executor: &mut PgSqlExecutor,
    meeting_id: &str,
    job_id: &str,
    guild_id: &str,
) -> Result<(), String> {
    executor.execute(
        "INSERT INTO meetings (id, guild_id, voice_channel_id, report_channel_id, started_by_user_id, status, error_message) VALUES ($1, $2, $3, $4, $5, 'failed', 'previous failure')",
        &[
            meeting_id.to_owned(),
            guild_id.to_owned(),
            format!("vc-{meeting_id}"),
            "report-1".to_owned(),
            "user-1".to_owned(),
        ],
    )?;
    executor.execute(
        "INSERT INTO jobs (id, meeting_id, job_type, status, retry_count, error_message, claim_token, leased_until, dead_lettered_at, canceled_at, cancel_reason) VALUES ($1, $2, 'summarize', 'failed', 3, 'previous failure', 'stale-token', NOW(), NOW(), NOW(), 'old cancellation')",
        &[job_id.to_owned(), meeting_id.to_owned()],
    )?;
    Ok(())
}

fn retry(
    executor: &mut PgSqlExecutor,
    job_id: &str,
    guild_id: &str,
    has_recoverable_audio: bool,
) -> Result<Vec<discord_transcript::infrastructure::sql_store::SqlRow>, String> {
    executor.query_rows(
        ADMIN_RETRY_JOB_SQL,
        &[
            job_id.to_owned(),
            guild_id.to_owned(),
            String::new(),
            has_recoverable_audio.to_string(),
        ],
    )
}

fn assert_meeting_job_state(
    executor: &mut PgSqlExecutor,
    meeting_id: &str,
    job_id: &str,
    meeting_status: &str,
    job_status: &str,
) -> Result<(), String> {
    let rows = executor.query_rows(
        "SELECT m.status=$3, (m.error_message IS NULL)=($3='stopping'), j.status=$4, j.retry_count=CASE WHEN $4='queued' THEN 0 ELSE 3 END, (j.error_message IS NULL)=($4='queued'), (j.claim_token IS NULL)=($4='queued') FROM meetings m JOIN jobs j ON j.meeting_id=m.id WHERE m.id=$1 AND j.id=$2",
        &[
            meeting_id.to_owned(),
            job_id.to_owned(),
            meeting_status.to_owned(),
            job_status.to_owned(),
        ],
    )?;
    if rows.first().is_none_or(|row| {
        row.iter()
            .any(|value| value.as_deref() != Some("true"))
    }) {
        return Err(format!("unexpected meeting/job state for {meeting_id}/{job_id}"));
    }
    Ok(())
}

fn assert_one_concurrent_retry_wins(database_url: &str, schema: &str) -> Result<(), String> {
    let barrier = Arc::new(Barrier::new(2));
    let handles = [0, 1].map(|_| {
        let database_url = database_url.to_owned();
        let schema = schema.to_owned();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || -> Result<usize, String> {
            let mut executor = executor_for(&database_url, &schema)?;
            barrier.wait();
            retry(&mut executor, "job-concurrent", "g1", true).map(|rows| rows.len())
        })
    });
    let counts = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .map_err(|_| "concurrent retry thread panicked".to_owned())?
        })
        .collect::<Result<Vec<_>, _>>()?;
    if counts.iter().sum::<usize>() != 1 {
        return Err(format!("exactly one concurrent retry must win, got {counts:?}"));
    }
    Ok(())
}
