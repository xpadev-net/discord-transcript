use discord_transcript::application::retention_cleanup::{
    RETENTION_DELETE_DEBUG_ARTIFACTS_SQL, RETENTION_DELETE_RAW_ARTIFACTS_SQL,
    RETENTION_EXPIRED_DEBUG_WORKSPACES_SQL,
    apply_retention_filesystem_cleanup, collect_retention_cleanup_plan,
};
use discord_transcript::domain::retention::RetentionPolicy;
use discord_transcript::infrastructure::s3::{ObjectStore, S3Error};
use discord_transcript::infrastructure::storage_s3::RecordingObjectStore;
use discord_transcript::infrastructure::sql_store::{PgSqlExecutor, SqlExecutor, SqlMeetingStore};
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Default)]
struct PrefixRecordingObjectStore {
    listed_prefixes: Mutex<Vec<String>>,
    deleted_keys: Mutex<Vec<String>>,
}

impl ObjectStore for PrefixRecordingObjectStore {
    fn put_object(&self, _key: &str, _body: &[u8], _content_type: &str) -> Result<(), S3Error> {
        Ok(())
    }

    fn head_object(&self, _key: &str) -> Result<bool, S3Error> {
        Ok(false)
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
        self.listed_prefixes
            .lock()
            .expect("prefix list lock")
            .push(prefix.to_owned());
        Ok(vec![format!("{prefix}chunk.wav")])
    }

    fn delete_keys(&self, keys: &[String]) -> Result<(), S3Error> {
        self.deleted_keys
            .lock()
            .expect("deleted keys lock")
            .extend(keys.iter().cloned());
        Ok(())
    }

    fn presigned_get_url(&self, key: &str, _ttl: Duration) -> String {
        format!("https://example.test/{key}")
    }

    fn endpoint_label(&self) -> String {
        "retention-test".to_owned()
    }
}

#[test]
fn postgres_raw_retention_uses_meeting_snapshot_ttl_for_all_raw_targets() {
    let database_url = match std::env::var("DISCORD_TRANSCRIPT_TEST_DATABASE_URL") {
        Ok(database_url) => database_url,
        Err(_) if std::env::var_os("CI").is_some() => {
            panic!("DISCORD_TRANSCRIPT_TEST_DATABASE_URL is required in CI")
        }
        Err(_) => {
            eprintln!("skipping real Postgres raw retention contract test; set DISCORD_TRANSCRIPT_TEST_DATABASE_URL");
            return;
        }
    };

    let schema = format!("dt_raw_retention_{}", uuid::Uuid::new_v4().simple());
    eprintln!("running real Postgres raw retention test in isolated schema {schema}");
    let mut migrator = PgSqlExecutor::connect(&database_url).expect("postgres connection");
    migrator
        .run_migration(&format!("CREATE SCHEMA {schema}; SET search_path TO {schema}"))
        .expect("test schema should be created and selected");
    SqlMeetingStore::new(migrator)
        .apply_pending_migrations()
        .expect("schema migrations should apply");

    let result = run_contract(&database_url, &schema);

    let mut cleanup = PgSqlExecutor::connect(&database_url).expect("cleanup connection");
    cleanup
        .run_migration(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .expect("test schema should be dropped");
    result.expect("raw retention contract should pass");
}

fn run_contract(database_url: &str, schema: &str) -> Result<(), String> {
    let mut executor = executor_for(database_url, schema)?;
    for (meeting_id, snapshot_ttl) in [
        ("m-long", Some(30)),
        ("m-expired", Some(7)),
        ("m-legacy", None),
        ("m-broken", Some(0)),
    ] {
        insert_terminal_meeting(&mut executor, meeting_id)?;
        if let Some(ttl) = snapshot_ttl {
            if ttl == 0 {
                executor.run_migration(
                    "ALTER TABLE meeting_effective_settings DROP CONSTRAINT meeting_effective_settings_retention_raw_audio_ttl_days_check",
                )?;
            }
            insert_settings_snapshot(&mut executor, meeting_id, ttl)?;
        }
        insert_raw_and_debug_artifacts(&mut executor, meeting_id)?;
    }

    let plan = collect_retention_cleanup_plan(&mut executor, RetentionPolicy::default());
    let raw_ids = sorted_ids(plan.raw_workspaces.iter().map(|row| row.meeting_id.as_str()));
    if raw_ids != ["m-expired", "m-legacy"] {
        return Err(format!("unexpected raw workspace targets: {raw_ids:?}"));
    }

    let debug_ids = executor.query_rows(
        RETENTION_EXPIRED_DEBUG_WORKSPACES_SQL,
        &["7".to_owned()],
    )?;
    if !debug_ids.is_empty() {
        return Err(format!("uncleaned meetings must not be debug-only targets: {debug_ids:?}"));
    }
    executor.execute(
        "UPDATE meetings SET retention_raw_cleaned_at=NOW() WHERE id IN ('m-long', 'm-expired', 'm-legacy', 'm-broken')",
        &[],
    )?;
    let debug_ids = executor.query_rows(
        RETENTION_EXPIRED_DEBUG_WORKSPACES_SQL,
        &["7".to_owned()],
    )?;
    let debug_ids = sorted_ids(debug_ids.iter().filter_map(|row| {
        row.first().and_then(Option::as_deref)
    }));
    if debug_ids != ["m-expired", "m-legacy"] {
        return Err(format!("unexpected debug workspace targets: {debug_ids:?}"));
    }

    let temp_dir = std::env::temp_dir().join(format!(
        "discord-transcript-raw-retention-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&temp_dir).map_err(|err| err.to_string())?;
    let layout = MeetingWorkspaceLayout::new(&temp_dir);
    for meeting_id in ["m-long", "m-expired", "m-legacy", "m-broken"] {
        let workspace = layout.for_meeting("g1", "vc1", meeting_id);
        std::fs::create_dir_all(workspace.audio_dir()).map_err(|err| err.to_string())?;
        std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav")
            .map_err(|err| err.to_string())?;
        std::fs::create_dir_all(layout.legacy_meeting_dir(meeting_id))
            .map_err(|err| err.to_string())?;
    }
    let remote = Arc::new(PrefixRecordingObjectStore::default());
    let remote_dyn: Arc<dyn ObjectStore> = remote.clone();
    let objects = RecordingObjectStore::new(remote_dyn, String::new(), 900, &temp_dir);
    let fs_report = apply_retention_filesystem_cleanup(&layout, &plan, Some(&objects))
        .map_err(|err| err.message)?;
    let cleaned_ids = sorted_ids(
        fs_report
            .raw_workspace_cleaned_meeting_ids
            .iter()
            .map(String::as_str),
    );
    if cleaned_ids != raw_ids {
        return Err(format!("filesystem and workspace targets differ: {cleaned_ids:?}"));
    }
    for meeting_id in ["m-long", "m-broken"] {
        let workspace = layout.for_meeting("g1", "vc1", meeting_id);
        if !workspace.audio_dir().exists() {
            return Err(format!("audio for protected {meeting_id} was removed"));
        }
    }
    let prefixes = remote
        .listed_prefixes
        .lock()
        .map_err(|_| "prefix list lock poisoned".to_owned())?
        .clone();
    for meeting_id in ["m-expired", "m-legacy"] {
        let expected_audio_prefix = layout
            .for_meeting("g1", "vc1", meeting_id)
            .audio_dir();
        let expected_audio_prefix = objects
            .object_prefix(&expected_audio_prefix)
        .expect("audio path should be in S3 storage root");
        if !prefixes.contains(&expected_audio_prefix) {
            return Err(format!("missing selected S3 prefix {expected_audio_prefix}"));
        }
        let expected_legacy_prefix = objects
            .object_prefix(&layout.legacy_meeting_dir(meeting_id))
            .expect("legacy audio path should be in S3 storage root");
        if !prefixes.contains(&expected_legacy_prefix) {
            return Err(format!("missing selected legacy S3 prefix {expected_legacy_prefix}"));
        }
    }
    if prefixes.iter().any(|prefix| prefix.contains("m-long") || prefix.contains("m-broken")) {
        return Err(format!("protected meeting S3 prefix was selected: {prefixes:?}"));
    }

    executor.execute(RETENTION_DELETE_RAW_ARTIFACTS_SQL, &["7".to_owned()])?;
    executor.execute(RETENTION_DELETE_DEBUG_ARTIFACTS_SQL, &["7".to_owned()])?;
    let remaining = executor.query_rows(
        "SELECT meeting_id, kind FROM artifacts ORDER BY meeting_id, kind",
        &[],
    )?;
    let remaining = remaining
        .iter()
        .filter_map(|row| {
            Some((row.first()?.as_deref()?, row.get(1)?.as_deref()?))
        })
        .collect::<Vec<_>>();
    if remaining != [("m-broken", "debug"), ("m-broken", "raw_audio"), ("m-long", "debug"), ("m-long", "raw_audio")] {
        return Err(format!("artifact targets did not match workspace/S3 targets: {remaining:?}"));
    }
    std::fs::remove_dir_all(&temp_dir).map_err(|err| err.to_string())?;
    Ok(())
}

fn executor_for(database_url: &str, schema: &str) -> Result<PgSqlExecutor, String> {
    let mut executor = PgSqlExecutor::connect(database_url)?;
    executor.run_migration(&format!("SET search_path TO {schema}"))?;
    Ok(executor)
}

fn insert_terminal_meeting(
    executor: &mut PgSqlExecutor,
    meeting_id: &str,
) -> Result<(), String> {
    executor.execute(
        "INSERT INTO meetings (id, guild_id, voice_channel_id, report_channel_id, started_by_user_id, status, stopped_at) VALUES ($1, 'g1', 'vc1', 'report', 'user1', 'posted', NOW() - INTERVAL '8 days')",
        &[meeting_id.to_owned()],
    )?;
    Ok(())
}

fn insert_settings_snapshot(
    executor: &mut PgSqlExecutor,
    meeting_id: &str,
    raw_ttl: u32,
) -> Result<(), String> {
    executor.execute(
        "INSERT INTO meeting_effective_settings (meeting_id, whisper_vad, whisper_beam_size, whisper_suppress_non_speech, whisper_temperature, whisper_resample_to_16k, auto_stop_grace_seconds, retention_raw_audio_ttl_days, retention_transcript_ttl_days, summary_enabled) VALUES ($1, TRUE, 5, FALSE, 0, FALSE, 60, $2::text::integer, 30, TRUE)",
        &[meeting_id.to_owned(), raw_ttl.to_string()],
    )?;
    Ok(())
}

fn insert_raw_and_debug_artifacts(
    executor: &mut PgSqlExecutor,
    meeting_id: &str,
) -> Result<(), String> {
    for (kind, suffix) in [("raw_audio", "raw"), ("debug", "debug_artifact")] {
        executor.execute(
            "INSERT INTO artifacts (id, meeting_id, kind, storage_url, size_bytes) VALUES ($1, $2, $3, 'test://artifact', 1)",
            &[
                format!("{meeting_id}-{suffix}"),
                meeting_id.to_owned(),
                kind.to_owned(),
            ],
        )?;
    }
    Ok(())
}

fn sorted_ids<'a>(ids: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut ids = ids.collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

