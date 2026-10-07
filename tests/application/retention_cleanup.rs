use discord_transcript::application::retention_cleanup::{
    RETENTION_DELETE_DEBUG_ARTIFACTS_SQL, RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL,
    RETENTION_DELETE_RAW_ARTIFACTS_SQL, RETENTION_DELETE_SUMMARIES_SQL,
    RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL, RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL,
    RETENTION_EXPIRED_DEBUG_WORKSPACES_SQL, RETENTION_EXPIRED_RAW_WORKSPACES_SQL,
    RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL, RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL,
    RETENTION_MARK_TRANSCRIPTS_DELETED_SQL, ExpiredWorkspaceRow, RetentionDeletionTargets,
    apply_manual_meeting_filesystem_delete, enforce_retention_policy,
    estimate_meeting_filesystem_usage, estimate_target_filesystem_usage,
};
use discord_transcript::domain::retention::RetentionPolicy;
use discord_transcript::infrastructure::s3::{ObjectStore, S3Error};
use discord_transcript::infrastructure::sql::{
    ADMIN_RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL, ADMIN_RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL,
};
use discord_transcript::infrastructure::sql_store::{FakeSqlExecutor, sql_row_from_strings};
use discord_transcript::infrastructure::storage_s3::RecordingObjectStore;
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

fn nonzero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value should be nonzero")
}

#[derive(Debug, Default)]
struct RemoteFakeObjectStore {
    lists: Mutex<Vec<String>>,
    deletes: Mutex<Vec<Vec<String>>>,
    list_result: Mutex<Vec<String>>,
    fail_deletes: Mutex<bool>,
}

impl ObjectStore for RemoteFakeObjectStore {
    fn put_object(&self, _key: &str, _body: &[u8], _content_type: &str) -> Result<(), S3Error> {
        Ok(())
    }

    fn head_object(&self, _key: &str) -> Result<bool, S3Error> {
        Ok(false)
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
        self.lists.lock().unwrap().push(prefix.to_owned());
        Ok(self
            .list_result
            .lock()
            .unwrap()
            .iter()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect())
    }

    fn delete_keys(&self, keys: &[String]) -> Result<(), S3Error> {
        self.deletes.lock().unwrap().push(keys.to_vec());
        if *self.fail_deletes.lock().unwrap() {
            return Err(S3Error::Status {
                status: 503,
                detail: "injected delete failure".to_owned(),
            });
        }
        Ok(())
    }

    fn presigned_get_url(&self, _key: &str, _ttl: Duration) -> String {
        "https://example.test/object".to_owned()
    }

    fn endpoint_label(&self) -> String {
        "fake".to_owned()
    }
}

/// Builds a `RecordingObjectStore` rooted at the layout's base dir so meeting
/// audio dirs resolve to the same key prefix as production wiring.
fn remote_object_store(
    layout: &MeetingWorkspaceLayout,
) -> (Arc<RemoteFakeObjectStore>, RecordingObjectStore) {
    let fake = Arc::new(RemoteFakeObjectStore::default());
    let dyn_store: Arc<dyn ObjectStore> = fake.clone();
    let base = layout
        .workspace_root()
        .parent()
        .expect("workspace root has a base dir")
        .to_path_buf();
    (
        fake,
        RecordingObjectStore::new(dyn_store, String::new(), 900, base),
    )
}

#[test]
fn retention_cleanup_removes_expired_raw_audio_debug_and_marks_transcripts() {
    let (_guard, layout) = temp_layout("raw_debug_transcripts");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav").expect("write audio");
    std::fs::write(workspace.debug_dir().join("summary_prompt.txt"), b"prompt")
        .expect("write debug");
    std::fs::create_dir_all(workspace.legacy_debug_dir()).expect("create legacy debug");
    std::fs::write(
        workspace.legacy_debug_dir().join("meeting_title.txt"),
        b"legacy title",
    )
    .expect("write legacy debug");
    std::fs::create_dir_all(workspace.agent_runs_debug_dir().join("failed-run-1"))
        .expect("write retained agent run debug dir");
    std::fs::write(
        workspace
            .agent_runs_debug_dir()
            .join("failed-run-1")
            .join("diagnostics.txt"),
        b"bounded diagnostics",
    )
    .expect("write retained agent run diagnostics");
    std::fs::create_dir_all(
        workspace
            .agent_workspace_parent_dir()
            .join("summary-failed-run")
            .join("output"),
    )
    .expect("write retained live agent workspace dir");
    std::fs::write(
        workspace
            .agent_workspace_parent_dir()
            .join("summary-failed-run")
            .join("output")
            .join("summary.md"),
        b"retained failed output",
    )
    .expect("write retained live agent workspace output");
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
    assert!(
        workspace
            .agent_runs_debug_dir()
            .join("failed-run-1")
            .join("diagnostics.txt")
            .is_file()
    );
    assert!(
        workspace
            .agent_workspace_parent_dir()
            .join("summary-failed-run")
            .join("output")
            .join("summary.md")
            .is_file()
    );

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, &["30"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
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

    let report = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default(), None)
        .expect("cleanup should succeed");

    assert_eq!(report.raw_workspaces_scanned, 1);
    assert_eq!(report.raw_audio_dirs_removed, 1);
    assert_eq!(report.legacy_meetings_cleaned, 1);
    assert_eq!(report.speaker_dirs_removed, 1);
    assert_eq!(report.context_dirs_removed, 1);
    assert_eq!(report.transcript_dirs_removed, 1);
    assert_eq!(report.empty_summary_dirs_removed, 1);
    assert_eq!(report.debug_dirs_removed, 2);
    assert_eq!(report.agent_workspace_dirs_removed, 1);
    assert_eq!(report.transcripts_marked_deleted, 3);
    assert_eq!(report.artifacts_deleted, 12);
    assert!(!workspace.audio_dir().exists());
    assert!(!workspace.debug_dir().exists());
    assert!(!workspace.legacy_debug_dir().exists());
    assert!(!workspace.agent_workspace_parent_dir().exists());
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
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
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
            raw_audio_ttl_days: nonzero(7),
            transcript_ttl_days: nonzero(30),
            summary_ttl_days: Some(nonzero(90)),
        }, None)
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
fn summary_retention_delete_clears_titles_atomically_when_summary_content_expires() {
    assert!(
        RETENTION_DELETE_SUMMARIES_SQL.contains("cleared_summary_titles AS"),
        "summary cleanup should clear meeting titles in the summary delete statement"
    );
    assert!(
        RETENTION_DELETE_SUMMARIES_SQL.contains("UPDATE meetings m"),
        "summary cleanup should update meetings.title as part of deleting summaries"
    );
    assert!(
        RETENTION_DELETE_SUMMARIES_SQL.contains("NOT EXISTS"),
        "title cleanup should keep titles while a non-expired summary remains"
    );
    assert!(
        RETENTION_DELETE_SUMMARIES_SQL.contains("FROM summaries active_s"),
        "title cleanup should check for active summary rows"
    );
    assert!(
        RETENTION_DELETE_SUMMARIES_SQL.contains("FROM summaries expired_s"),
        "title cleanup should only clear titles when expired summary content exists"
    );
    assert!(
        RETENTION_DELETE_SUMMARIES_SQL.contains("m.stopped_at IS NOT NULL"),
        "title cleanup should also cover stopped old meetings whose summary rows are already gone"
    );
}

#[test]
fn retention_cleanup_revisits_raw_cleaned_meetings_for_legacy_debug_artifacts() {
    let (_guard, layout) = temp_layout("debug_rerun_raw_cleaned");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    std::fs::create_dir_all(workspace.legacy_debug_dir()).expect("create legacy debug");
    std::fs::write(
        workspace.legacy_debug_dir().join("meeting_title.txt"),
        b"legacy title",
    )
    .expect("write legacy debug");

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_DEBUG_WORKSPACES_SQL, &["7"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let report = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default(), None)
        .expect("debug cleanup should succeed");

    assert_eq!(report.raw_workspaces_scanned, 0);
    assert_eq!(report.debug_dirs_removed, 1);
    assert!(!workspace.legacy_debug_dir().exists());
    assert!(
        executor
            .executed
            .iter()
            .any(|(sql, params)| sql == RETENTION_EXPIRED_DEBUG_WORKSPACES_SQL
                && params == &vec!["7".to_owned()])
    );
}

#[test]
fn retention_cleanup_is_idempotent_for_missing_workspace_files() {
    let (_guard, layout) = temp_layout("idempotent");
    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let report = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default(), None)
        .expect("missing directories should be ignored");

    assert_eq!(report.raw_workspaces_scanned, 1);
    assert_eq!(report.raw_audio_dirs_removed, 0);
    assert_eq!(report.legacy_meetings_cleaned, 0);
    assert_eq!(report.speaker_dirs_removed, 0);
    assert_eq!(report.context_dirs_removed, 0);
    assert_eq!(report.transcript_dirs_removed, 0);
    assert_eq!(report.empty_summary_dirs_removed, 0);
    assert_eq!(report.summary_dirs_removed, 0);
    assert_eq!(report.debug_dirs_removed, 0);
}

#[test]
fn retention_workspace_queries_remain_visible_after_db_tombstones_or_deletes() {
    for sql in [
        RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL,
        ADMIN_RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL,
    ] {
        let expired_block = sql
            .split_once("FROM transcripts expired_t")
            .expect("expired transcript block should exist")
            .1
            .split_once("OR (m.stopped_at IS NOT NULL")
            .expect("stopped-at retry fallback should exist")
            .0;
        assert!(
            !expired_block.contains("expired_t.is_deleted = FALSE"),
            "deleted transcript rows must not hide retryable workspace cleanup"
        );
    }

    for sql in [
        RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL,
        ADMIN_RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL,
    ] {
        assert!(
            sql.contains("OR EXISTS (\n    SELECT 1\n    FROM summaries expired_s")
                || sql.contains("OR EXISTS (\n      SELECT 1\n      FROM summaries expired_s"),
            "summary workspace cleanup must still find stopped old meetings after summary rows are deleted"
        );
        assert!(
            sql.contains("m.stopped_at IS NOT NULL"),
            "summary workspace cleanup needs a stopped-at fallback for deleted/no-row summaries"
        );
    }
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
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );
    executor.execute_result.insert(
        query_key(RETENTION_MARK_TRANSCRIPTS_DELETED_SQL, &["30"]),
        3,
    );

    let err = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default(), None)
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
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, &["30"]),
        vec![sql_row_from_strings(vec![
            "m2".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL, &["90"]),
        vec![sql_row_from_strings(vec![
            "m2".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let err = enforce_retention_policy(
        &mut executor,
        &layout,
        RetentionPolicy {
            raw_audio_ttl_days: nonzero(7),
            transcript_ttl_days: nonzero(30),
            summary_ttl_days: Some(nonzero(90)),
        }, None)
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
fn retention_cleanup_can_rerun_after_transcript_and_summary_filesystem_failure() {
    let (_guard, layout) = temp_layout("fs_failure_rerun_transcript_summary");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    std::fs::create_dir_all(workspace.root()).expect("create workspace root");
    std::fs::write(workspace.transcript_dir(), b"not a directory")
        .expect("write transcript path as file");
    std::fs::write(workspace.summary_dir(), b"not a directory").expect("write summary path as file");

    let mut first_executor = FakeSqlExecutor::default();
    first_executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, &["30"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );
    first_executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL, &["90"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let err = enforce_retention_policy(
        &mut first_executor,
        &layout,
        RetentionPolicy {
            raw_audio_ttl_days: nonzero(7),
            transcript_ttl_days: nonzero(30),
            summary_ttl_days: Some(nonzero(90)),
        }, None)
    .expect_err("filesystem cleanup should fail before paths become removable");

    assert!(err.message.contains("failed to remove"));
    assert_eq!(err.report.transcript_dirs_removed, 0);
    assert_eq!(err.report.summary_dirs_removed, 0);
    assert!(
        first_executor
            .executed
            .iter()
            .any(|(sql, params)| sql == RETENTION_MARK_TRANSCRIPTS_DELETED_SQL
                && params == &vec!["30".to_owned()]),
        "database tombstone should still run after filesystem failure"
    );
    assert!(
        first_executor
            .executed
            .iter()
            .any(|(sql, params)| sql == RETENTION_DELETE_SUMMARIES_SQL
                && params == &vec!["90".to_owned()]),
        "summary row delete should still run after filesystem failure"
    );

    std::fs::remove_file(workspace.transcript_dir()).expect("remove obstructing transcript file");
    std::fs::create_dir_all(workspace.transcript_dir()).expect("create transcript dir");
    std::fs::write(workspace.transcript_dir().join("transcript.md"), b"transcript")
        .expect("write remaining transcript");
    std::fs::remove_file(workspace.summary_dir()).expect("remove obstructing summary file");
    std::fs::create_dir_all(workspace.summary_dir()).expect("create summary dir");
    std::fs::write(workspace.summary_dir().join("summary.md"), b"summary")
        .expect("write remaining summary");

    let mut retry_executor = FakeSqlExecutor::default();
    retry_executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, &["30"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );
    retry_executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL, &["90"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let retry_report = enforce_retention_policy(
        &mut retry_executor,
        &layout,
        RetentionPolicy {
            raw_audio_ttl_days: nonzero(7),
            transcript_ttl_days: nonzero(30),
            summary_ttl_days: Some(nonzero(90)),
        }, None)
    .expect("retry should remove the remaining workspace files");

    assert_eq!(retry_report.transcript_dirs_removed, 1);
    assert_eq!(retry_report.summary_dirs_removed, 1);
    assert!(!workspace.transcript_dir().exists());
    assert!(!workspace.summary_dir().exists());
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
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );
    executor.execute_error.insert(
        query_key(RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL, &[]),
        "database unavailable".to_owned(),
    );
    executor.execute_result.insert(
        query_key(RETENTION_MARK_TRANSCRIPTS_DELETED_SQL, &["30"]),
        3,
    );

    let err = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default(), None)
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
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );
    executor.query_rows_error.insert(
        query_key(RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL, &["30"]),
        "transcript query unavailable".to_owned(),
    );

    let err = enforce_retention_policy(&mut executor, &layout, RetentionPolicy::default(), None)
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

#[test]
fn manual_meeting_delete_estimates_and_removes_selected_targets_only() {
    let (_guard, layout) = temp_layout("manual_delete_targets");
    let meeting = ExpiredWorkspaceRow {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
    };
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"raw").expect("write raw");
    std::fs::write(workspace.speakers_dir().join("u1.wav"), b"speaker").expect("write speaker");
    std::fs::write(workspace.context_dir().join("manifest.json"), b"{}").expect("write context");
    std::fs::write(workspace.transcript_dir().join("transcript.md"), b"transcript")
        .expect("write transcript");
    std::fs::write(workspace.summary_dir().join("summary.md"), b"summary")
        .expect("write summary");
    std::fs::write(workspace.debug_dir().join("debug.txt"), b"debug").expect("write debug");
    std::fs::create_dir_all(workspace.legacy_debug_dir()).expect("create legacy debug");
    std::fs::write(workspace.legacy_debug_dir().join("legacy.txt"), b"legacy debug")
        .expect("write legacy debug");

    let usage = estimate_meeting_filesystem_usage(&layout, &meeting).expect("estimate usage");
    assert!(usage.raw_audio_bytes > 0);
    assert!(usage.transcript_bytes > 0);
    assert!(usage.summary_bytes > 0);
    assert!(usage.debug_bytes > 0);

    let targets = RetentionDeletionTargets {
        raw_audio: true,
        transcript: false,
        summary: true,
        debug: false,
    };
    let target_usage =
        estimate_target_filesystem_usage(&layout, &meeting, targets).expect("estimate targets");
    assert_eq!(target_usage.raw_audio_bytes, usage.raw_audio_bytes);
    assert_eq!(target_usage.transcript_bytes, 0);
    assert_eq!(target_usage.summary_bytes, usage.summary_bytes);
    assert_eq!(target_usage.debug_bytes, 0);

    let report = apply_manual_meeting_filesystem_delete(&layout, &meeting, targets, None)
        .expect("manual delete succeeds");
    assert_eq!(report.raw_workspaces_scanned, 1);
    assert_eq!(
        report.raw_workspace_cleaned_meeting_ids,
        vec!["m1".to_owned()]
    );
    assert_eq!(report.raw_audio_dirs_removed, 1);
    assert_eq!(report.speaker_dirs_removed, 1);
    assert_eq!(report.context_dirs_removed, 1);
    assert_eq!(report.summary_dirs_removed, 1);
    assert_eq!(report.transcript_dirs_removed, 0);
    assert_eq!(report.debug_dirs_removed, 0);
    assert_eq!(report.agent_workspace_dirs_removed, 0);
    assert!(workspace.transcript_dir().exists());
    assert!(workspace.debug_dir().exists());
    assert!(workspace.legacy_debug_dir().exists());
    assert!(!workspace.audio_dir().exists());
    assert!(!workspace.speakers_dir().exists());
    assert!(!workspace.context_dir().exists());
    assert!(!workspace.summary_dir().exists());

    let debug_targets = RetentionDeletionTargets {
        raw_audio: false,
        transcript: false,
        summary: false,
        debug: true,
    };
    let report = apply_manual_meeting_filesystem_delete(&layout, &meeting, debug_targets, None)
        .expect("manual debug delete succeeds");
    assert_eq!(report.debug_dirs_removed, 2);
    assert_eq!(report.agent_workspace_dirs_removed, 0);
    assert!(!workspace.debug_dir().exists());
    assert!(!workspace.legacy_debug_dir().exists());
}

#[test]
fn retention_cleanup_deletes_remote_objects_for_expired_raw_workspace() {
    let (_guard, layout) = temp_layout("remote_raw");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav").expect("write audio");

    let (fake, objects) = remote_object_store(&layout);
    *fake.list_result.lock().unwrap() = vec![
        "workspaces/g1/vc1/m1/audio/chunk.wav".to_owned(),
        "workspaces/g1/vc1/m1/audio/mixdown.wav".to_owned(),
    ];

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let report = enforce_retention_policy(
        &mut executor,
        &layout,
        RetentionPolicy::default(),
        Some(&objects),
    )
    .expect("cleanup should succeed");

    assert_eq!(report.raw_workspaces_scanned, 1);
    assert_eq!(report.raw_audio_dirs_removed, 1);
    assert_eq!(report.remote_objects_deleted, 2);
    assert_eq!(
        report.raw_workspace_cleaned_meeting_ids,
        vec!["m1".to_owned()]
    );
    let lists = fake.lists.lock().unwrap();
    assert_eq!(lists.len(), 2);
    assert!(lists[0].ends_with("g1/vc1/m1/audio/"));
    assert_eq!(lists[1], "m1/");
    drop(lists);
    assert_eq!(
        *fake.deletes.lock().unwrap(),
        vec![vec![
            "workspaces/g1/vc1/m1/audio/chunk.wav".to_owned(),
            "workspaces/g1/vc1/m1/audio/mixdown.wav".to_owned(),
        ]]
    );
}

#[test]
fn retention_cleanup_deletes_legacy_recording_objects() {
    let (_guard, layout) = temp_layout("remote_legacy");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav").expect("write audio");

    let legacy_dir = layout.legacy_meeting_dir("m1");
    std::fs::create_dir_all(legacy_dir.join("speakers")).expect("create legacy speakers");
    std::fs::write(legacy_dir.join("chunk.wav"), b"legacy wav").expect("write legacy wav");
    std::fs::write(legacy_dir.join("mixdown.wav"), b"legacy mixdown").expect("write mixdown");
    std::fs::write(legacy_dir.join("speakers/u1.wav"), b"speaker").expect("write speaker");
    std::fs::write(legacy_dir.join("transcript.md"), b"transcript").expect("write transcript");

    let (fake, objects) = remote_object_store(&layout);
    *fake.list_result.lock().unwrap() = vec![
        "workspaces/g1/vc1/m1/audio/chunk.wav".to_owned(),
        "m1/chunk.wav".to_owned(),
        "m1/mixdown.wav".to_owned(),
        "m1/speakers/u1.wav".to_owned(),
        "m1/transcript.md".to_owned(),
    ];

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let report = enforce_retention_policy(
        &mut executor,
        &layout,
        RetentionPolicy::default(),
        Some(&objects),
    )
    .expect("cleanup should succeed");

    assert_eq!(
        report.remote_objects_deleted,
        4,
        "audio prefix keys plus legacy wav/speakers objects"
    );
    assert_eq!(
        *fake.deletes.lock().unwrap(),
        vec![
            vec!["workspaces/g1/vc1/m1/audio/chunk.wav".to_owned()],
            vec![
                "m1/chunk.wav".to_owned(),
                "m1/mixdown.wav".to_owned(),
                "m1/speakers/u1.wav".to_owned(),
            ],
        ],
        "legacy recording keys are deleted but m1/transcript.md is kept"
    );
}

#[test]
fn retention_cleanup_deletes_remote_objects_on_manual_raw_delete() {
    let (_guard, layout) = temp_layout("remote_manual");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav").expect("write audio");

    let (fake, objects) = remote_object_store(&layout);
    *fake.list_result.lock().unwrap() = vec!["workspaces/g1/vc1/m1/audio/chunk.wav".to_owned()];

    let meeting = ExpiredWorkspaceRow {
        meeting_id: "m1".to_owned(),
        guild_id: "g1".to_owned(),
        voice_channel_id: "vc1".to_owned(),
    };
    let targets = RetentionDeletionTargets {
        raw_audio: true,
        transcript: false,
        summary: false,
        debug: false,
    };
    let report = apply_manual_meeting_filesystem_delete(
        &layout,
        &meeting,
        targets,
        Some(&objects),
    )
    .expect("manual delete succeeds");

    assert_eq!(report.remote_objects_deleted, 1);
    assert_eq!(
        report.raw_workspace_cleaned_meeting_ids,
        vec!["m1".to_owned()]
    );
    assert_eq!(
        *fake.deletes.lock().unwrap(),
        vec![vec!["workspaces/g1/vc1/m1/audio/chunk.wav".to_owned()]]
    );
}

#[test]
fn retention_cleanup_remote_delete_runs_even_after_local_failure() {
    let (_guard, layout) = temp_layout("remote_after_local_failure");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    std::fs::create_dir_all(workspace.audio_dir()).expect("create audio dir");
    // Block local speaker removal (which also skips the parent audio
    // cleanup); the remote copy must still go.
    std::fs::write(workspace.speakers_dir(), b"not a directory").expect("write speakers as file");

    let (fake, objects) = remote_object_store(&layout);
    *fake.list_result.lock().unwrap() = vec!["workspaces/g1/vc1/m1/audio/chunk.wav".to_owned()];

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let err = enforce_retention_policy(
        &mut executor,
        &layout,
        RetentionPolicy::default(),
        Some(&objects),
    )
    .expect_err("local filesystem failure should still fail the meeting");

    assert!(err.message.contains("failed to remove"));
    assert_eq!(err.report.remote_objects_deleted, 1);
    assert_eq!(
        *fake.deletes.lock().unwrap(),
        vec![vec!["workspaces/g1/vc1/m1/audio/chunk.wav".to_owned()]],
        "remote deletion must run even when local staging removal fails"
    );
    assert!(
        err.report.raw_workspace_cleaned_meeting_ids.is_empty(),
        "a meeting with local errors is not marked cleaned"
    );
}

#[test]
fn retention_cleanup_remote_delete_failure_blocks_clean_marker() {
    let (_guard, layout) = temp_layout("remote_failure");
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    workspace.ensure_base_dirs().expect("create workspace");
    std::fs::write(workspace.audio_dir().join("chunk.wav"), b"wav").expect("write audio");

    let (fake, objects) = remote_object_store(&layout);
    *fake.list_result.lock().unwrap() = vec!["workspaces/g1/vc1/m1/audio/chunk.wav".to_owned()];
    *fake.fail_deletes.lock().unwrap() = true;

    let mut executor = FakeSqlExecutor::default();
    executor.query_rows_result.insert(
        query_key(RETENTION_EXPIRED_RAW_WORKSPACES_SQL, &["7"]),
        vec![sql_row_from_strings(vec![
            "m1".to_owned(),
            "g1".to_owned(),
            "vc1".to_owned(),
        ])],
    );

    let err = enforce_retention_policy(
        &mut executor,
        &layout,
        RetentionPolicy::default(),
        Some(&objects),
    )
    .expect_err("remote delete failure should fail the meeting");

    assert!(err.message.contains("object store delete failed"));
    assert_eq!(err.report.remote_objects_deleted, 0);
    assert_eq!(
        *fake.deletes.lock().unwrap(),
        vec![vec!["workspaces/g1/vc1/m1/audio/chunk.wav".to_owned()]],
        "the injected failure exercised a delete of stored audio"
    );
    assert!(
        err.report.raw_workspace_cleaned_meeting_ids.is_empty(),
        "a remote failure must not mark the meeting cleaned"
    );
}
