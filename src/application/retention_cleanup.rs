use crate::domain::retention::RetentionPolicy;
use crate::infrastructure::sql_store::SqlExecutor;
use crate::infrastructure::workspace::MeetingWorkspaceLayout;
use std::fs;
use std::io;
use std::path::Path;
use tracing::warn;

// Raw-audio and transcript workspace queries share the same filters today, but
// stay separate so their retention scopes can diverge without changing callers.
pub const RETENTION_EXPIRED_RAW_WORKSPACES_SQL: &str = r#"
SELECT id, guild_id, voice_channel_id
FROM meetings
WHERE stopped_at IS NOT NULL
  AND stopped_at < NOW() - (($1 || ' days')::interval)
  AND status IN ('posted', 'failed', 'aborted')
  AND retention_raw_cleaned_at IS NULL
"#;

// See RETENTION_EXPIRED_RAW_WORKSPACES_SQL for why this intentionally mirrors
// the raw-audio workspace query instead of aliasing it.
pub const RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL: &str = r#"
SELECT m.id, m.guild_id, m.voice_channel_id
FROM meetings m
WHERE m.status IN ('posted', 'failed', 'aborted')
  AND (
    NOT EXISTS (
      SELECT 1
      FROM transcripts active_t
      WHERE active_t.meeting_id = m.id
        AND active_t.is_deleted = FALSE
        AND active_t.created_at >= NOW() - (($1 || ' days')::interval)
    )
  )
  AND (
    EXISTS (
      SELECT 1
      FROM transcripts expired_t
      WHERE expired_t.meeting_id = m.id
        AND expired_t.created_at < NOW() - (($1 || ' days')::interval)
    )
    OR (m.stopped_at IS NOT NULL AND m.stopped_at < NOW() - (($1 || ' days')::interval))
  )
"#;

// Summary workspace cleanup is gated by RETENTION_SUMMARY_TTL_DAYS and uses
// its own constant so future summary-specific filters can diverge.
pub const RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL: &str = r#"
SELECT m.id, m.guild_id, m.voice_channel_id
FROM meetings m
WHERE m.status IN ('posted', 'failed', 'aborted')
  AND NOT EXISTS (
    SELECT 1
    FROM summaries active_s
    WHERE active_s.meeting_id = m.id
      AND active_s.created_at >= NOW() - (($1 || ' days')::interval)
  )
  AND (
    (
      m.stopped_at IS NOT NULL
      AND m.stopped_at < NOW() - (($1 || ' days')::interval)
    )
    OR EXISTS (
    SELECT 1
    FROM summaries expired_s
    WHERE expired_s.meeting_id = m.id
      AND expired_s.created_at < NOW() - (($1 || ' days')::interval)
    )
  )
"#;

pub const RETENTION_MARK_TRANSCRIPTS_DELETED_SQL: &str = r#"
UPDATE transcripts t
SET is_deleted=TRUE
FROM meetings m
WHERE t.meeting_id = m.id
  AND t.is_deleted=FALSE
  AND t.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const RETENTION_DELETE_SUMMARIES_SQL: &str = r#"
DELETE FROM summaries s
USING meetings m
WHERE s.meeting_id = m.id
  AND s.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND a.expires_at IS NOT NULL
  AND a.expires_at <= NOW()
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const RETENTION_DELETE_RAW_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND a.kind IN ('raw_audio', 'audio', 'mixdown_audio', 'speaker_audio')
  AND m.stopped_at IS NOT NULL
  AND m.stopped_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND a.kind IN ('transcript', 'masked_transcript')
  AND a.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND a.kind IN ('summary', 'summary_markdown')
  AND a.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const RETENTION_DELETE_DEBUG_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts a
USING meetings m
WHERE a.meeting_id = m.id
  AND a.kind IN ('debug', 'debug_artifact', 'whisper_debug')
  AND m.stopped_at IS NOT NULL
  AND m.stopped_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const RETENTION_MARK_RAW_WORKSPACE_CLEANED_SQL: &str = r#"
UPDATE meetings
SET retention_raw_cleaned_at=NOW(),
    updated_at=NOW()
WHERE id=$1
  AND retention_raw_cleaned_at IS NULL
"#;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionCleanupReport {
    pub raw_workspaces_scanned: usize,
    pub raw_audio_dirs_removed: usize,
    pub legacy_meetings_cleaned: usize,
    pub raw_workspaces_marked_cleaned: u64,
    pub raw_workspace_cleaned_meeting_ids: Vec<String>,
    pub speaker_dirs_removed: usize,
    pub context_dirs_removed: usize,
    pub transcript_dirs_removed: usize,
    pub empty_summary_dirs_removed: usize,
    pub summary_dirs_removed: usize,
    pub debug_dirs_removed: usize,
    pub agent_workspace_dirs_removed: usize,
    pub transcripts_marked_deleted: u64,
    pub summaries_deleted: u64,
    pub artifacts_deleted: u64,
}

impl RetentionCleanupReport {
    pub fn merge(&mut self, other: RetentionCleanupReport) {
        self.raw_workspaces_scanned += other.raw_workspaces_scanned;
        self.raw_audio_dirs_removed += other.raw_audio_dirs_removed;
        self.legacy_meetings_cleaned += other.legacy_meetings_cleaned;
        self.raw_workspaces_marked_cleaned += other.raw_workspaces_marked_cleaned;
        self.raw_workspace_cleaned_meeting_ids
            .extend(other.raw_workspace_cleaned_meeting_ids);
        self.speaker_dirs_removed += other.speaker_dirs_removed;
        self.context_dirs_removed += other.context_dirs_removed;
        self.transcript_dirs_removed += other.transcript_dirs_removed;
        self.empty_summary_dirs_removed += other.empty_summary_dirs_removed;
        self.summary_dirs_removed += other.summary_dirs_removed;
        self.debug_dirs_removed += other.debug_dirs_removed;
        self.agent_workspace_dirs_removed += other.agent_workspace_dirs_removed;
        self.transcripts_marked_deleted += other.transcripts_marked_deleted;
        self.summaries_deleted += other.summaries_deleted;
        self.artifacts_deleted += other.artifacts_deleted;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionCleanupError {
    pub report: Box<RetentionCleanupReport>,
    pub message: String,
}

impl std::fmt::Display for RetentionCleanupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RetentionCleanupError {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionCleanupPlan {
    pub raw_workspaces: Vec<ExpiredWorkspaceRow>,
    pub transcript_workspaces: Vec<ExpiredWorkspaceRow>,
    pub summary_workspaces: Vec<ExpiredWorkspaceRow>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionDeletionTargets {
    pub raw_audio: bool,
    pub transcript: bool,
    pub summary: bool,
    pub debug: bool,
}

impl RetentionDeletionTargets {
    pub fn any(self) -> bool {
        self.raw_audio || self.transcript || self.summary || self.debug
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionStorageUsage {
    pub raw_audio_bytes: u64,
    pub transcript_bytes: u64,
    pub summary_bytes: u64,
    pub debug_bytes: u64,
}

impl RetentionStorageUsage {
    pub fn total_bytes(&self) -> u64 {
        self.raw_audio_bytes
            .saturating_add(self.transcript_bytes)
            .saturating_add(self.summary_bytes)
            .saturating_add(self.debug_bytes)
    }
}

/// Enforces retention synchronously, including blocking filesystem deletion.
/// Async callers should run the filesystem phase through `spawn_blocking` like
/// `run_startup_retention_cleanup` does.
pub fn enforce_retention_policy<E: SqlExecutor>(
    executor: &mut E,
    workspace_layout: &MeetingWorkspaceLayout,
    policy: RetentionPolicy,
) -> Result<RetentionCleanupReport, RetentionCleanupError> {
    let plan = collect_retention_cleanup_plan(executor, policy);
    let plan_errors = plan.errors.clone();
    let mut report = RetentionCleanupReport::default();
    let filesystem_result = apply_retention_filesystem_cleanup(workspace_layout, &plan);
    let filesystem_error = match filesystem_result {
        Ok(filesystem_report) => {
            report.merge(filesystem_report);
            None
        }
        Err(err) => {
            report.merge(*err.report);
            Some(err.message)
        }
    };
    let raw_workspace_cleaned_meeting_ids = report.raw_workspace_cleaned_meeting_ids.clone();
    let database_error = match apply_retention_database_cleanup(
        executor,
        policy,
        &raw_workspace_cleaned_meeting_ids,
    ) {
        Ok(database_report) => {
            report.merge(database_report);
            None
        }
        Err(err) => {
            report.merge(*err.report);
            Some(err.message)
        }
    };
    let mut messages = plan_errors;
    if let Some(fs_err) = filesystem_error {
        messages.push(fs_err);
    }
    if let Some(db_err) = database_error {
        messages.push(format!("database cleanup failed: {db_err}"));
    }
    let message = if messages.is_empty() {
        None
    } else {
        Some(messages.join("; "))
    };
    if let Some(message) = message {
        Err(RetentionCleanupError {
            report: Box::new(report),
            message,
        })
    } else {
        Ok(report)
    }
}

pub fn collect_retention_cleanup_plan<E: SqlExecutor>(
    executor: &mut E,
    policy: RetentionPolicy,
) -> RetentionCleanupPlan {
    let raw_ttl = policy.raw_audio_ttl_days.get().to_string();
    let transcript_ttl = policy.transcript_ttl_days.get().to_string();
    let mut errors = Vec::new();

    let raw_workspaces = collect_workspace_rows(
        executor,
        RETENTION_EXPIRED_RAW_WORKSPACES_SQL,
        std::slice::from_ref(&raw_ttl),
        &mut errors,
    );

    let transcript_workspaces = collect_workspace_rows(
        executor,
        RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL,
        std::slice::from_ref(&transcript_ttl),
        &mut errors,
    );

    let summary_workspaces = if let Some(summary_ttl_days) = policy.summary_ttl_days {
        let summary_ttl = summary_ttl_days.get().to_string();
        collect_workspace_rows(
            executor,
            RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL,
            std::slice::from_ref(&summary_ttl),
            &mut errors,
        )
    } else {
        Vec::new()
    };

    RetentionCleanupPlan {
        raw_workspaces,
        transcript_workspaces,
        summary_workspaces,
        errors,
    }
}

pub fn apply_retention_filesystem_cleanup(
    workspace_layout: &MeetingWorkspaceLayout,
    plan: &RetentionCleanupPlan,
) -> Result<RetentionCleanupReport, RetentionCleanupError> {
    let mut report = RetentionCleanupReport::default();
    let mut errors = Vec::new();
    for meeting in &plan.raw_workspaces {
        let error_count_before = errors.len();
        report.raw_workspaces_scanned += 1;
        let workspace = workspace_layout.for_meeting(
            &meeting.guild_id,
            &meeting.voice_channel_id,
            &meeting.meeting_id,
        );
        let speaker_cleanup = remove_dir_if_present(&workspace.speakers_dir());
        let speaker_cleanup_failed = speaker_cleanup.is_err();
        record_cleanup_result(&mut errors, speaker_cleanup, || {
            report.speaker_dirs_removed += 1
        });
        if speaker_cleanup_failed {
            warn!(
                meeting_id = %meeting.meeting_id,
                path = %workspace.speakers_dir().display(),
                skipped_path = %workspace.audio_dir().display(),
                "skipping parent audio cleanup after speaker directory cleanup failed"
            );
        } else {
            record_cleanup_result(
                &mut errors,
                remove_dir_if_present(&workspace.audio_dir()),
                || report.raw_audio_dirs_removed += 1,
            );
        }
        record_cleanup_result(
            &mut errors,
            remove_legacy_raw_audio(&workspace_layout.legacy_meeting_dir(&meeting.meeting_id)),
            || report.legacy_meetings_cleaned += 1,
        );
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.context_dir()),
            || report.context_dirs_removed += 1,
        );
        record_cleanup_result(
            &mut errors,
            remove_empty_dir_if_present(&workspace.summary_dir()),
            || report.empty_summary_dirs_removed += 1,
        );
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.debug_dir()),
            || report.debug_dirs_removed += 1,
        );
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.agent_workspace_parent_dir()),
            || report.agent_workspace_dirs_removed += 1,
        );
        if errors.len() == error_count_before {
            report
                .raw_workspace_cleaned_meeting_ids
                .push(meeting.meeting_id.clone());
        }
    }

    for meeting in &plan.transcript_workspaces {
        let workspace = workspace_layout.for_meeting(
            &meeting.guild_id,
            &meeting.voice_channel_id,
            &meeting.meeting_id,
        );
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.transcript_dir()),
            || report.transcript_dirs_removed += 1,
        );
    }

    for meeting in &plan.summary_workspaces {
        let workspace = workspace_layout.for_meeting(
            &meeting.guild_id,
            &meeting.voice_channel_id,
            &meeting.meeting_id,
        );
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.summary_dir()),
            || report.summary_dirs_removed += 1,
        );
    }

    if errors.is_empty() {
        Ok(report)
    } else {
        Err(RetentionCleanupError {
            report: Box::new(report),
            message: errors.join("; "),
        })
    }
}

pub fn estimate_meeting_filesystem_usage(
    workspace_layout: &MeetingWorkspaceLayout,
    meeting: &ExpiredWorkspaceRow,
) -> Result<RetentionStorageUsage, String> {
    let workspace = workspace_layout.for_meeting(
        &meeting.guild_id,
        &meeting.voice_channel_id,
        &meeting.meeting_id,
    );
    Ok(RetentionStorageUsage {
        raw_audio_bytes: dir_size_if_present(&workspace.audio_dir())?
            .saturating_add(dir_size_if_present(&workspace.context_dir())?)
            .saturating_add(legacy_raw_audio_size(
                &workspace_layout.legacy_meeting_dir(&meeting.meeting_id),
            )?),
        transcript_bytes: dir_size_if_present(&workspace.transcript_dir())?,
        summary_bytes: dir_size_if_present(&workspace.summary_dir())?,
        debug_bytes: dir_size_if_present(&workspace.debug_dir())?.saturating_add(
            dir_size_if_present(&workspace.agent_workspace_parent_dir())?,
        ),
    })
}

pub fn estimate_target_filesystem_usage(
    workspace_layout: &MeetingWorkspaceLayout,
    meeting: &ExpiredWorkspaceRow,
    targets: RetentionDeletionTargets,
) -> Result<RetentionStorageUsage, String> {
    let usage = estimate_meeting_filesystem_usage(workspace_layout, meeting)?;
    Ok(RetentionStorageUsage {
        raw_audio_bytes: if targets.raw_audio {
            usage.raw_audio_bytes
        } else {
            0
        },
        transcript_bytes: if targets.transcript {
            usage.transcript_bytes
        } else {
            0
        },
        summary_bytes: if targets.summary {
            usage.summary_bytes
        } else {
            0
        },
        debug_bytes: if targets.debug { usage.debug_bytes } else { 0 },
    })
}

pub fn apply_manual_meeting_filesystem_delete(
    workspace_layout: &MeetingWorkspaceLayout,
    meeting: &ExpiredWorkspaceRow,
    targets: RetentionDeletionTargets,
) -> Result<RetentionCleanupReport, RetentionCleanupError> {
    let workspace = workspace_layout.for_meeting(
        &meeting.guild_id,
        &meeting.voice_channel_id,
        &meeting.meeting_id,
    );
    let mut report = RetentionCleanupReport::default();
    let mut errors = Vec::new();

    if targets.raw_audio {
        report.raw_workspaces_scanned = 1;
        let error_count_before = errors.len();
        let speaker_cleanup = remove_dir_if_present(&workspace.speakers_dir());
        let speaker_cleanup_failed = speaker_cleanup.is_err();
        record_cleanup_result(&mut errors, speaker_cleanup, || {
            report.speaker_dirs_removed += 1
        });
        if speaker_cleanup_failed {
            warn!(
                meeting_id = %meeting.meeting_id,
                path = %workspace.speakers_dir().display(),
                skipped_path = %workspace.audio_dir().display(),
                "skipping parent audio cleanup after speaker directory cleanup failed"
            );
        } else {
            record_cleanup_result(
                &mut errors,
                remove_dir_if_present(&workspace.audio_dir()),
                || report.raw_audio_dirs_removed += 1,
            );
        }
        record_cleanup_result(
            &mut errors,
            remove_legacy_raw_audio(&workspace_layout.legacy_meeting_dir(&meeting.meeting_id)),
            || report.legacy_meetings_cleaned += 1,
        );
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.context_dir()),
            || report.context_dirs_removed += 1,
        );
        if errors.len() == error_count_before {
            report
                .raw_workspace_cleaned_meeting_ids
                .push(meeting.meeting_id.clone());
        }
    }

    if targets.transcript {
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.transcript_dir()),
            || report.transcript_dirs_removed += 1,
        );
    }

    if targets.summary {
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.summary_dir()),
            || report.summary_dirs_removed += 1,
        );
    }

    if targets.debug {
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.debug_dir()),
            || report.debug_dirs_removed += 1,
        );
        record_cleanup_result(
            &mut errors,
            remove_dir_if_present(&workspace.agent_workspace_parent_dir()),
            || report.agent_workspace_dirs_removed += 1,
        );
    }

    if errors.is_empty() {
        Ok(report)
    } else {
        Err(RetentionCleanupError {
            report: Box::new(report),
            message: errors.join("; "),
        })
    }
}

pub fn apply_retention_database_cleanup<E: SqlExecutor>(
    executor: &mut E,
    policy: RetentionPolicy,
    raw_workspace_cleaned_meeting_ids: &[String],
) -> Result<RetentionCleanupReport, RetentionCleanupError> {
    let mut report = RetentionCleanupReport::default();
    let mut errors = Vec::new();
    let raw_ttl = policy.raw_audio_ttl_days.get().to_string();
    let transcript_ttl = policy.transcript_ttl_days.get().to_string();

    for meeting_id in raw_workspace_cleaned_meeting_ids {
        match executor.execute(
            RETENTION_MARK_RAW_WORKSPACE_CLEANED_SQL,
            std::slice::from_ref(meeting_id),
        ) {
            Ok(count) => report.raw_workspaces_marked_cleaned += count,
            Err(err) => errors.push(err),
        }
    }

    match executor.execute(
        RETENTION_MARK_TRANSCRIPTS_DELETED_SQL,
        std::slice::from_ref(&transcript_ttl),
    ) {
        Ok(count) => report.transcripts_marked_deleted += count,
        Err(err) => errors.push(err),
    }
    match executor.execute(RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL, &[]) {
        Ok(count) => report.artifacts_deleted += count,
        Err(err) => errors.push(err),
    }
    match executor.execute(
        RETENTION_DELETE_RAW_ARTIFACTS_SQL,
        std::slice::from_ref(&raw_ttl),
    ) {
        Ok(count) => report.artifacts_deleted += count,
        Err(err) => errors.push(err),
    }
    match executor.execute(
        RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL,
        std::slice::from_ref(&transcript_ttl),
    ) {
        Ok(count) => report.artifacts_deleted += count,
        Err(err) => errors.push(err),
    }
    // Debug artifacts intentionally share the raw-audio TTL; add a
    // dedicated RETENTION_DEBUG_TTL_DAYS if independent control is needed.
    match executor.execute(
        RETENTION_DELETE_DEBUG_ARTIFACTS_SQL,
        std::slice::from_ref(&raw_ttl),
    ) {
        Ok(count) => report.artifacts_deleted += count,
        Err(err) => errors.push(err),
    }

    if let Some(summary_ttl_days) = policy.summary_ttl_days {
        let summary_ttl = summary_ttl_days.get().to_string();
        match executor.execute(
            RETENTION_DELETE_SUMMARIES_SQL,
            std::slice::from_ref(&summary_ttl),
        ) {
            Ok(count) => report.summaries_deleted += count,
            Err(err) => errors.push(err),
        }
        match executor.execute(
            RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL,
            std::slice::from_ref(&summary_ttl),
        ) {
            Ok(count) => report.artifacts_deleted += count,
            Err(err) => errors.push(err),
        }
    }

    if errors.is_empty() {
        Ok(report)
    } else {
        Err(RetentionCleanupError {
            report: Box::new(report),
            message: errors.join("; "),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredWorkspaceRow {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
}

fn parse_workspace_row(
    row: &crate::infrastructure::sql_store::SqlRow,
) -> Result<ExpiredWorkspaceRow, String> {
    if row.len() < 3 {
        return Err(format!(
            "invalid retention workspace row length: {}",
            row.len()
        ));
    }
    let require = |idx: usize, field: &str| -> Result<String, String> {
        row.get(idx)
            .and_then(|v| v.clone())
            .ok_or_else(|| format!("{field} is NULL"))
    };
    Ok(ExpiredWorkspaceRow {
        meeting_id: require(0, "meeting_id")?,
        guild_id: require(1, "guild_id")?,
        voice_channel_id: require(2, "voice_channel_id")?,
    })
}

fn collect_workspace_rows<E: SqlExecutor>(
    executor: &mut E,
    sql: &str,
    params: &[String],
    errors: &mut Vec<String>,
) -> Vec<ExpiredWorkspaceRow> {
    let rows = match executor.query_rows(sql, params) {
        Ok(rows) => rows,
        Err(err) => {
            errors.push(err);
            return Vec::new();
        }
    };

    rows.into_iter()
        .filter_map(|row| match parse_workspace_row(&row) {
            Ok(parsed) => Some(parsed),
            Err(err) => {
                errors.push(err);
                None
            }
        })
        .collect()
}

fn remove_dir_if_present(path: &Path) -> Result<bool, String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("failed to remove {}: {err}", path.display())),
    }
}

fn record_cleanup_result(
    errors: &mut Vec<String>,
    result: Result<bool, String>,
    mut on_removed: impl FnMut(),
) {
    match result {
        Ok(true) => on_removed(),
        Ok(false) => {}
        Err(err) => errors.push(err),
    }
}

fn remove_legacy_raw_audio(meeting_dir: &Path) -> Result<bool, String> {
    let mut removed = false;
    let mut errors = Vec::new();
    match fs::read_dir(meeting_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        errors.push(format!("failed to read {}: {err}", meeting_dir.display()));
                        continue;
                    }
                };
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("wav") {
                    match fs::remove_file(&path) {
                        Ok(()) => removed = true,
                        Err(err) => {
                            errors.push(format!("failed to remove {}: {err}", path.display()))
                        }
                    }
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => errors.push(format!("failed to read {}: {err}", meeting_dir.display())),
    }

    match remove_dir_if_present(&meeting_dir.join("speakers")) {
        Ok(true) => removed = true,
        Ok(false) => {}
        Err(err) => errors.push(err),
    }
    match remove_empty_dir_if_present(meeting_dir) {
        Ok(true) => removed = true,
        Ok(false) => {}
        Err(err) => errors.push(err),
    }
    if errors.is_empty() {
        Ok(removed)
    } else {
        Err(errors.join("; "))
    }
}

fn remove_empty_dir_if_present(path: &Path) -> Result<bool, String> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
        Err(err) => Err(format!("failed to remove empty {}: {err}", path.display())),
    }
}

fn dir_size_if_present(path: &Path) -> Result<u64, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(format!("failed to stat {}: {err}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut size = 0_u64;
    let entries =
        fs::read_dir(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read {}: {err}", path.display()))?;
        size = size.saturating_add(dir_size_if_present(&entry.path())?);
    }
    Ok(size)
}

fn legacy_raw_audio_size(meeting_dir: &Path) -> Result<u64, String> {
    let mut size = 0_u64;
    let entries = match fs::read_dir(meeting_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(format!("failed to read {}: {err}", meeting_dir.display())),
    };

    for entry in entries {
        let entry =
            entry.map_err(|err| format!("failed to read {}: {err}", meeting_dir.display()))?;
        let path = entry.path();
        let is_legacy_raw_audio = path.extension().and_then(|ext| ext.to_str()) == Some("wav")
            || path.file_name().and_then(|name| name.to_str()) == Some("speakers");
        if is_legacy_raw_audio {
            size = size.saturating_add(dir_size_if_present(&path)?);
        }
    }
    Ok(size)
}
