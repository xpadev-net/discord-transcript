use discord_transcript::application::retention_cleanup::{
    apply_retention_database_cleanup, apply_retention_filesystem_cleanup,
    collect_retention_cleanup_plan,
};
use discord_transcript::domain::retention::RetentionPolicy;
use discord_transcript::infrastructure::s3::{ObjectStore, S3Error};
use discord_transcript::infrastructure::sql_store::{PgSqlExecutor, SqlExecutor, SqlMeetingStore};
use discord_transcript::infrastructure::storage_s3::RecordingObjectStore;
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Default)]
struct PrefixRecorder(Mutex<Vec<String>>);

impl ObjectStore for PrefixRecorder {
    fn put_object(&self, _key: &str, _body: &[u8], _content_type: &str) -> Result<(), S3Error> {
        Ok(())
    }

    fn head_object(&self, _key: &str) -> Result<bool, S3Error> {
        Ok(false)
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
        self.0.lock().map_err(|_| S3Error::Http("prefix lock poisoned".to_owned()))?.push(prefix.to_owned());
        Ok(vec![format!("{prefix}chunk.wav")])
    }

    fn delete_keys(&self, _keys: &[String]) -> Result<(), S3Error> {
        Ok(())
    }

    fn presigned_get_url(&self, key: &str, _ttl: Duration) -> String {
        format!("https://example.test/{key}")
    }

    fn endpoint_label(&self) -> String {
        "guild-retention-test".to_owned()
    }
}

#[test]
fn postgres_retention_plan_filesystem_and_database_are_guild_scoped() {
    let database_url = match std::env::var("DISCORD_TRANSCRIPT_TEST_DATABASE_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("CI").is_some() => {
            panic!("DISCORD_TRANSCRIPT_TEST_DATABASE_URL is required in CI")
        }
        Err(_) => {
            eprintln!("skipping real Postgres guild retention contract test; set DISCORD_TRANSCRIPT_TEST_DATABASE_URL");
            return;
        }
    };
    let schema = format!("dt_guild_retention_{}", uuid::Uuid::new_v4().simple());
    let mut migrator = PgSqlExecutor::connect(&database_url).expect("postgres connection");
    migrator
        .run_migration(&format!("CREATE SCHEMA {schema}; SET search_path TO {schema}"))
        .expect("isolated schema should be selected");
    SqlMeetingStore::new(migrator)
        .apply_pending_migrations()
        .expect("migrations should apply");
    let result = run_contract(&database_url, &schema);
    let mut cleanup = PgSqlExecutor::connect(&database_url).expect("cleanup connection");
    cleanup
        .run_migration(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .expect("isolated schema should be dropped");
    result.expect("guild retention contract should pass");
}

fn run_contract(database_url: &str, schema: &str) -> Result<(), String> {
    let mut db = PgSqlExecutor::connect(database_url)?;
    db.run_migration(&format!("SET search_path TO {schema}"))?;
    for (id, guild_id) in [("meeting-1", "guild-1"), ("meeting-2", "guild-2")] {
        db.execute(
            "INSERT INTO meetings (id,guild_id,voice_channel_id,report_channel_id,started_by_user_id,title,status,stopped_at) VALUES ($1,$2,'voice','report','user','title','posted',NOW()-INTERVAL '35 days')",
            &[id.to_owned(), guild_id.to_owned()],
        )?;
        db.execute(
            "INSERT INTO meeting_effective_settings (meeting_id,whisper_vad,whisper_beam_size,whisper_suppress_non_speech,whisper_temperature,whisper_resample_to_16k,auto_stop_grace_seconds,retention_raw_audio_ttl_days,retention_transcript_ttl_days,retention_summary_ttl_days,summary_enabled) VALUES ($1,TRUE,5,FALSE,0,FALSE,60,7,7,7,TRUE)",
            &[id.to_owned()],
        )?;
        db.execute(
            "INSERT INTO transcripts (id,meeting_id,speaker_id,start_ms,end_ms,text,created_at) VALUES ($1,$2,'speaker',0,1,'expired',NOW()-INTERVAL '35 days')",
            &[format!("transcript-{id}"), id.to_owned()],
        )?;
        db.execute(
            "INSERT INTO summaries (id,meeting_id,version,markdown,created_at) VALUES ($1,$2,1,'expired',NOW()-INTERVAL '35 days')",
            &[format!("summary-{id}"), id.to_owned()],
        )?;
        for kind in ["raw_audio", "debug", "transcript", "summary", "other"] {
            db.execute(
                "INSERT INTO artifacts (id,meeting_id,kind,storage_url,size_bytes,expires_at,created_at) VALUES ($1,$2,$3,'test://object',10,NOW()-INTERVAL '1 day',NOW()-INTERVAL '35 days')",
                &[format!("artifact-{kind}-{id}"), id.to_owned(), kind.to_owned()],
            )?;
        }
    }

    let policy = RetentionPolicy {
        raw_audio_ttl_days: NonZeroU32::new(7).expect("nonzero"),
        transcript_ttl_days: NonZeroU32::new(7).expect("nonzero"),
        summary_ttl_days: Some(NonZeroU32::new(7).expect("nonzero")),
    };
    let plan = collect_retention_cleanup_plan(&mut db, "guild-1", policy);
    if !plan.errors.is_empty() {
        return Err(format!("plan collection errors: {:?}", plan.errors));
    }
    for rows in [
        &plan.raw_workspaces,
        &plan.debug_workspaces,
        &plan.transcript_workspaces,
        &plan.summary_workspaces,
    ] {
        if rows.iter().any(|row| row.guild_id != "guild-1") {
            return Err(format!("cleanup plan crossed guild scope: {rows:?}"));
        }
    }
    if plan.raw_workspaces.len() != 1
        || plan.raw_workspaces[0].meeting_id != "meeting-1"
        || !plan.debug_workspaces.is_empty()
        || plan.transcript_workspaces.len() != 1
        || plan.summary_workspaces.len() != 1
    {
        return Err(format!("unexpected scoped targets: {plan:?}"));
    }

    let temp_dir = std::env::temp_dir().join(format!("discord-transcript-guild-retention-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&temp_dir).map_err(|err| err.to_string())?;
    let layout = MeetingWorkspaceLayout::new(&temp_dir);
    for (id, guild_id) in [("meeting-1", "guild-1"), ("meeting-2", "guild-2")] {
        let workspace = layout.for_meeting(guild_id, "voice", id);
        workspace.ensure_base_dirs().map_err(|err| err.to_string())?;
        std::fs::write(workspace.audio_dir().join("chunk.wav"), b"audio").map_err(|err| err.to_string())?;
        std::fs::write(workspace.transcript_dir().join("transcript.json"), b"transcript").map_err(|err| err.to_string())?;
        std::fs::write(workspace.summary_dir().join("summary.md"), b"summary").map_err(|err| err.to_string())?;
        std::fs::write(workspace.debug_dir().join("debug.json"), b"debug").map_err(|err| err.to_string())?;
    }
    let remote = Arc::new(PrefixRecorder::default());
    let remote_dyn: Arc<dyn ObjectStore> = remote.clone();
    let objects = RecordingObjectStore::new(remote_dyn, String::new(), 900, &temp_dir);
    let fs_report = apply_retention_filesystem_cleanup(&layout, &plan, Some(&objects))
        .map_err(|err| err.message)?;
    if fs_report.raw_workspace_cleaned_meeting_ids != ["meeting-1"] {
        return Err(format!("filesystem cleanup crossed guild scope: {:?}", fs_report.raw_workspace_cleaned_meeting_ids));
    }
    let prefixes = remote.0.lock().map_err(|_| "prefix lock poisoned".to_owned())?.clone();
    if !prefixes.iter().any(|prefix| prefix.contains("meeting-1"))
        || prefixes.iter().any(|prefix| prefix.contains("meeting-2"))
    {
        return Err(format!("S3 cleanup crossed guild scope: {prefixes:?}"));
    }
    let protected = layout.for_meeting("guild-2", "voice", "meeting-2");
    if !protected.audio_dir().exists()
        || !protected.transcript_dir().exists()
        || !protected.summary_dir().exists()
        || !protected.debug_dir().exists()
    {
        return Err("other guild workspace was modified".to_owned());
    }

    apply_retention_database_cleanup(
        &mut db,
        "guild-1",
        policy,
        &fs_report.raw_workspace_cleaned_meeting_ids,
    )
    .map_err(|err| err.message)?;
    let rows = db.query_rows(
        "SELECT m.id,m.title,m.retention_raw_cleaned_at IS NOT NULL,t.is_deleted,(SELECT COUNT(*) FROM summaries s WHERE s.meeting_id=m.id),(SELECT COUNT(*) FROM artifacts a WHERE a.meeting_id=m.id) FROM meetings m JOIN transcripts t ON t.meeting_id=m.id ORDER BY m.id",
        &[],
    )?;
    let values = rows.iter().map(|row| row.iter().map(|cell| cell.as_deref()).collect::<Vec<_>>()).collect::<Vec<_>>();
    if values != [
        vec![Some("meeting-1"), None, Some("true"), Some("true"), Some("0"), Some("0")],
        vec![Some("meeting-2"), Some("title"), Some("false"), Some("false"), Some("1"), Some("5")],
    ] {
        return Err(format!("database cleanup crossed guild scope: {values:?}"));
    }
    std::fs::remove_dir_all(&temp_dir).map_err(|err| err.to_string())?;
    Ok(())
}
