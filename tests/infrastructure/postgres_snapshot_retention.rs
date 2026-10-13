use discord_transcript::application::retention_cleanup::{
    RETENTION_DELETE_SUMMARIES_SQL, RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL,
    RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL, RETENTION_MARK_TRANSCRIPTS_DELETED_SQL,
    collect_retention_cleanup_plan,
};
use discord_transcript::domain::retention::RetentionPolicy;
use discord_transcript::infrastructure::sql_store::{PgSqlExecutor, SqlExecutor, SqlMeetingStore};

#[test]
fn postgres_transcript_and_summary_retention_use_meeting_snapshots() {
    let database_url = match std::env::var("DISCORD_TRANSCRIPT_TEST_DATABASE_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("CI").is_some() => {
            panic!("DISCORD_TRANSCRIPT_TEST_DATABASE_URL is required in CI")
        }
        Err(_) => {
            eprintln!("skipping real Postgres snapshot retention contract test; set DISCORD_TRANSCRIPT_TEST_DATABASE_URL");
            return;
        }
    };
    let schema = format!("dt_snapshot_retention_{}", uuid::Uuid::new_v4().simple());
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
    result.expect("snapshot retention contract should pass");
}

fn run_contract(database_url: &str, schema: &str) -> Result<(), String> {
    let mut db = PgSqlExecutor::connect(database_url)?;
    db.run_migration(&format!("SET search_path TO {schema}"))?;
    for (id, status, transcript_ttl, summary_ttl) in [
        ("long", "posted", Some(60), Some(60)),
        ("expired", "posted", Some(7), Some(7)),
        ("unlimited", "posted", Some(7), None),
        ("legacy", "posted", None, None),
        ("processing", "recording", Some(1), Some(1)),
        ("mixed", "posted", Some(7), Some(7)),
        ("stop-null", "failed", Some(7), Some(7)),
    ] {
        db.execute(
            "INSERT INTO meetings (id,guild_id,voice_channel_id,report_channel_id,started_by_user_id,title,status,stopped_at) VALUES ($1,'g','v','r','u','title',$2,CASE WHEN $2='posted' THEN NOW()-INTERVAL '35 days' ELSE NULL END)",
            &[id.to_owned(), status.to_owned()],
        )?;
        if let Some(transcript_ttl) = transcript_ttl {
            db.execute(
                "INSERT INTO meeting_effective_settings (meeting_id,whisper_vad,whisper_beam_size,whisper_suppress_non_speech,whisper_temperature,whisper_resample_to_16k,auto_stop_grace_seconds,retention_raw_audio_ttl_days,retention_transcript_ttl_days,retention_summary_ttl_days,summary_enabled) VALUES ($1,TRUE,5,FALSE,0,FALSE,60,7,$2::text::integer,NULLIF($3,'')::integer,TRUE)",
                &[id.to_owned(), transcript_ttl.to_string(), summary_ttl.map_or(String::new(), |n| n.to_string())],
            )?;
            if summary_ttl.is_none() {
                db.execute("UPDATE meeting_effective_settings SET retention_summary_ttl_days=NULL WHERE meeting_id=$1", &[id.to_owned()])?;
            }
        }
        db.execute(
            "INSERT INTO transcripts (id,meeting_id,speaker_id,start_ms,end_ms,text,created_at) VALUES ($1,$2,'s',0,1,'old',NOW()-INTERVAL '35 days')",
            &[format!("t-{id}"), id.to_owned()],
        )?;
        db.execute(
            "INSERT INTO summaries (id,meeting_id,version,markdown,created_at) VALUES ($1,$2,1,'old',NOW()-INTERVAL '35 days')",
            &[format!("s-{id}"), id.to_owned()],
        )?;
        if id == "mixed" {
            db.execute(
                "INSERT INTO summaries (id,meeting_id,version,markdown,created_at) VALUES ('s-mixed-new',$1,2,'current',NOW())",
                &[id.to_owned()],
            )?;
            db.execute(
                "INSERT INTO artifacts (id,meeting_id,kind,storage_url,size_bytes,created_at) VALUES ('a-summary-new-mixed',$1,'summary','test://artifact',1,NOW())",
                &[id.to_owned()],
            )?;
        }
        for kind in ["transcript", "summary"] {
            db.execute(
                "INSERT INTO artifacts (id,meeting_id,kind,storage_url,size_bytes,created_at) VALUES ($1,$2,$3,'test://artifact',1,NOW()-INTERVAL '35 days')",
                &[format!("a-{kind}-{id}"), id.to_owned(), kind.to_owned()],
            )?;
        }
    }

    let plan = collect_retention_cleanup_plan(&mut db, RetentionPolicy::default());
    let ids = |rows: &[discord_transcript::application::retention_cleanup::ExpiredWorkspaceRow]| {
        let mut ids = rows.iter().map(|row| row.meeting_id.clone()).collect::<Vec<_>>();
        ids.sort();
        ids
    };
    if ids(&plan.transcript_workspaces) != ["expired", "legacy", "mixed", "stop-null", "unlimited"] {
        return Err(format!("unexpected transcript workspaces: {:?}", ids(&plan.transcript_workspaces)));
    }
    if ids(&plan.summary_workspaces) != ["expired", "stop-null"] {
        return Err(format!("unexpected summary workspaces: {:?}", ids(&plan.summary_workspaces)));
    }
    db.execute(RETENTION_MARK_TRANSCRIPTS_DELETED_SQL, &["30".to_owned()])?;
    db.execute(RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL, &["30".to_owned()])?;
    db.execute(RETENTION_DELETE_SUMMARIES_SQL, &["0".to_owned()])?;
    db.execute(RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL, &["0".to_owned()])?;
    let transcript_rows = db.query_rows("SELECT meeting_id FROM transcripts WHERE is_deleted ORDER BY meeting_id", &[])?;
    let transcript_ids = transcript_rows.iter().filter_map(|row| row.first()?.as_deref()).collect::<Vec<_>>();
    if transcript_ids != ["expired", "legacy", "mixed", "stop-null", "unlimited"] {
        return Err(format!("unexpected deleted transcripts: {transcript_ids:?}"));
    }
    let remaining = db.query_rows("SELECT meeting_id,kind FROM artifacts ORDER BY meeting_id,kind", &[])?;
    let remaining = remaining.iter().filter_map(|row| Some((row.first()?.as_deref()?, row.get(1)?.as_deref()?))).collect::<Vec<_>>();
    if remaining != [
        ("legacy", "summary"),
        ("long", "summary"),
        ("long", "transcript"),
        ("mixed", "summary"),
        ("processing", "summary"),
        ("processing", "transcript"),
        ("unlimited", "summary"),
    ] {
        return Err(format!("unexpected remaining artifacts: {remaining:?}"));
    }
    let remaining_summaries = db.query_rows("SELECT meeting_id FROM summaries ORDER BY meeting_id", &[])?;
    let summary_ids = remaining_summaries.iter().filter_map(|row| row.first()?.as_deref()).collect::<Vec<_>>();
    if summary_ids != ["legacy", "long", "mixed", "processing", "unlimited"] {
        return Err(format!("unexpected remaining summaries: {summary_ids:?}"));
    }
    let titles = db.query_rows("SELECT id FROM meetings WHERE title IS NULL ORDER BY id", &[])?;
    let cleared_title_ids = titles.iter().filter_map(|row| row.first()?.as_deref()).collect::<Vec<_>>();
    if cleared_title_ids != ["expired", "stop-null"] {
        return Err(format!("unexpected cleared summary titles: {cleared_title_ids:?}"));
    }
    Ok(())
}
