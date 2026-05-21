use discord_transcript::application::retention_cleanup::{
    RETENTION_DELETE_DEBUG_ARTIFACTS_SQL, RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL,
    RETENTION_DELETE_RAW_ARTIFACTS_SQL, RETENTION_DELETE_SUMMARIES_SQL,
    RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL, RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL,
    RETENTION_EXPIRED_RAW_WORKSPACES_SQL, RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL,
    RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, RETENTION_MARK_TRANSCRIPTS_DELETED_SQL,
    enforce_retention_policy,
};
use discord_transcript::domain::retention::RetentionPolicy;
use discord_transcript::infrastructure::sql_store::FakeSqlExecutor;
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::path::PathBuf;

struct TempWorkspaceGuard {
    base: PathBuf,
}

impl Drop for TempWorkspaceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn temp_layout(test_name: &str) -> (TempWorkspaceGuard, MeetingWorkspaceLayout) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!(
        "discord_transcript_retention_cleanup_{test_name}_{nanos}"
    ));
    let layout = MeetingWorkspaceLayout::new(&base);
    (TempWorkspaceGuard { base }, layout)
}

fn query_key(sql: &str, params: &[&str]) -> String {
    format!("{}|{}", sql, params.join("\u{1f}"))
}

#[test]
fn retention_cleanup_removes_expired_raw_audio_debug_and_marks_transcripts() {
    let (_guard, layout) = temp_layout("raw_debug_transcripts");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav").expect("write audio");
    std::fs::write(workspace.debug_dir().join("summary_prompt.txt"), b"prompt")
        .expect("write debug");
    std::fs::write(workspace.speakers_dir().join("u1_speaker.wav"), b"speaker")
        .expect("write speaker");
    std::fs::write(workspace.context_dir().join("vc_text.json"), b"{}").expect("write context");
    std::fs::write(workspace.masked_transcript_path(), b"masked").expect("write transcript");
    std::fs::write(workspace.transcript_manifest_path(), b"{}").expect("write manifest");
    let legacy_dir = layout.legacy_meeting_dir("m1");
    std::fs::create_dir_all(legacy_dir.join("speakers")).expect("create legacy speakers");
    std::fs::write(legacy_dir.join("mixdown.wav"), b"legacy").expect("write legacy mixdown");
    std::fs::write(legacy_dir.join("speakers").join("u1_speaker.wav"), b"speaker")
        .expect("write legacy speaker");

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![vec!["m1".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, &["30"]),
        vec![vec!["m1".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );
    executor.execute_result.insert(
        query_key(RETENTION_MARK_TRANSCRIPTS_DELETED_SQL, &["30"]),
        3,
    );
    executor
        .execute_result
        .insert(query_key(RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL, &[]), 1);
    executor
        .execute_result
        .insert(query_key(RETENTION_DELETE_RAW_ARTIFACTS_SQL, &["7"]), 2);
    executor.execute_result.insert(
        query_key(RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL, &["30"]),
        4,
    );
    executor
        .execute_result
        .insert(query_key(RETENTION_DELETE_DEBUG_ARTIFACTS_SQL, &["7"]), 5);

    let report = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default())
        .expect("cleanup should succeed");

    assert_eq!(report.raw_workspaces_scanned, 1);
    assert_eq!(report.raw_audio_dirs_removed, 1);
    assert_eq!(report.legacy_meetings_cleaned, 1);
    assert_eq!(report.speaker_dirs_removed, 1);
    assert_eq!(report.context_dirs_removed, 1);
    assert_eq!(report.transcript_dirs_removed, 1);
    assert_eq!(report.debug_dirs_removed, 1);
    assert_eq!(report.transcripts_marked_deleted, 3);
    assert_eq!(report.artifacts_deleted, 12);
    assert!(!workspace.audio_dir().exists());
    assert!(!workspace.debug_dir().exists());
    assert!(!workspace.speakers_dir().exists());
    assert!(!workspace.context_dir().exists());
    assert!(!workspace.transcript_dir().exists());
    assert!(!legacy_dir.join("mixdown.wav").exists());
    assert!(!legacy_dir.join("speakers").exists());
    assert!(!legacy_dir.exists());
    assert!(
        executor
            .executed
            .iter()
            .any(|(sql, params)| sql == RETENTION_MARK_TRANSCRIPTS_DELETED_SQL
                && params == &vec!["30".to_owned()])
    );
}

#[test]
fn retention_cleanup_applies_summary_ttl_when_configured() {
    let (_guard, layout) = temp_layout("summary_ttl");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.summary_dir().join("summary.md"), b"summary")
        .expect("write summary");
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL, &["90"]),
        vec![vec!["m1".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );
    executor.execute_result.insert(
        query_key(RETENTION_DELETE_SUMMARIES_SQL, &["90"]),
        6,
    );
    executor.execute_result.insert(
        query_key(RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL, &["90"]),
        7,
    );

    let report = enforce_retention_policy(
        &mut executor,
        &layout,
        RetentionPolicy {
            raw_audio_ttl_days: 7,
            transcript_ttl_days: 30,
            summary_ttl_days: Some(90),
        },
    )
    .expect("cleanup should succeed");

    assert_eq!(report.summaries_deleted, 6);
    assert_eq!(report.summary_dirs_removed, 1);
    // Four unregistered artifact-delete queries each return FakeSqlExecutor's
    // default of 1; only the summary-artifact query (7) is explicitly set.
    assert_eq!(report.artifacts_deleted, 11); // 1 + 1 + 1 + 1 + 7
    assert!(!workspace.summary_dir().exists());
    assert!(
        executor
            .executed
            .iter()
            .any(|(sql, params)| sql == RETENTION_DELETE_SUMMARIES_SQL
                && params == &vec!["90".to_owned()])
    );
}

#[test]
fn retention_cleanup_is_idempotent_for_missing_workspace_files() {
    let (_guard, layout) = temp_layout("idempotent");
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![vec!["m1".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );

    let report = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default())
        .expect("missing directories should be ignored");

    assert_eq!(report.raw_workspaces_scanned, 1);
    assert_eq!(report.raw_audio_dirs_removed, 0);
    assert_eq!(report.legacy_meetings_cleaned, 0);
    assert_eq!(report.speaker_dirs_removed, 0);
    assert_eq!(report.context_dirs_removed, 0);
    assert_eq!(report.transcript_dirs_removed, 0);
    assert_eq!(report.summary_dirs_removed, 0);
    assert_eq!(report.debug_dirs_removed, 0);
}

#[test]
fn retention_cleanup_runs_database_phase_when_filesystem_cleanup_fails() {
    let (_guard, layout) = temp_layout("fs_failure_keeps_db_cleanup");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    std::fs::create_dir_all(workspace.root()).expect("create workspace root");
    std::fs::write(workspace.audio_dir(), b"not a directory").expect("write audio path as file");

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![vec!["m1".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );
    executor.execute_result.insert(
        query_key(RETENTION_MARK_TRANSCRIPTS_DELETED_SQL, &["30"]),
        3,
    );

    let err = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default())
        .expect_err("filesystem cleanup should fail after database cleanup runs");

    assert!(err.message.contains("failed to remove"));
    assert!(
        executor
            .executed
            .iter()
            .any(|(sql, params)| sql == RETENTION_MARK_TRANSCRIPTS_DELETED_SQL
                && params == &vec!["30".to_owned()])
    );
}

#[test]
fn retention_cleanup_continues_filesystem_phase_after_meeting_error() {
    let (_guard, layout) = temp_layout("fs_failure_continues");
    let failed = layout.for_meeting("g1", "vc1", "m1");
    std::fs::create_dir_all(failed.root()).expect("create failed workspace root");
    std::fs::write(failed.audio_dir(), b"not a directory").expect("write audio path as file");

    let retained = layout.for_meeting("g1", "vc1", "m2");
    retained.ensure_base_dirs().expect("create retained workspace");
    std::fs::write(retained.masked_transcript_path(), b"masked").expect("write transcript");
    std::fs::write(retained.summary_dir().join("summary.md"), b"summary")
        .expect("write summary");

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![vec!["m1".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, &["30"]),
        vec![vec!["m2".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL, &["90"]),
        vec![vec!["m2".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );

    let err = enforce_retention_policy(
        &mut executor,
        &layout,
        RetentionPolicy {
            raw_audio_ttl_days: 7,
            transcript_ttl_days: 30,
            summary_ttl_days: Some(90),
        },
    )
    .expect_err("filesystem cleanup should report the failed meeting");

    assert!(err.message.contains("failed to remove"));
    assert_eq!(err.report.transcript_dirs_removed, 1);
    assert_eq!(err.report.summary_dirs_removed, 1);
    assert!(!retained.transcript_dir().exists());
    assert!(!retained.summary_dir().exists());
    assert!(
        executor
            .executed
            .iter()
            .any(|(sql, params)| sql == RETENTION_DELETE_SUMMARIES_SQL
                && params == &vec!["90".to_owned()])
    );
}

#[test]
fn retention_cleanup_preserves_partial_report_when_database_cleanup_fails() {
    let (_guard, layout) = temp_layout("db_failure_partial_report");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav").expect("write audio");

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![vec!["m1".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );
    executor.execute_error.insert(
        query_key(RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL, &[]),
        "database unavailable".to_owned(),
    );
    executor.execute_result.insert(
        query_key(RETENTION_MARK_TRANSCRIPTS_DELETED_SQL, &["30"]),
        3,
    );

    let err = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default())
        .expect_err("database cleanup should fail after filesystem cleanup runs");

    assert!(err.message.contains("database cleanup failed"));
    assert_eq!(err.report.raw_audio_dirs_removed, 1);
    assert_eq!(err.report.transcripts_marked_deleted, 3);
    assert!(!workspace.audio_dir().exists());
}

#[test]
fn retention_cleanup_uses_partial_plan_when_one_workspace_query_fails() {
    let (_guard, layout) = temp_layout("partial_plan_query_failure");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav").expect("write audio");

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![vec!["m1".to_owned(), "g1".to_owned(), "vc1".to_owned()]],
    );
    executor.query_rows_error.insert(
        query_key(RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, &["30"]),
        "transcript query unavailable".to_owned(),
    );

    let err = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default())
        .expect_err("plan query error should be reported after partial cleanup");

    assert!(err.message.contains("transcript query unavailable"));
    assert_eq!(err.report.raw_audio_dirs_removed, 1);
    assert!(!workspace.audio_dir().exists());
    assert!(
        executor
            .executed
            .iter()
            .any(|(sql, params)| sql == RETENTION_MARK_TRANSCRIPTS_DELETED_SQL
                && params == &vec!["30".to_owned()])
    );
}
