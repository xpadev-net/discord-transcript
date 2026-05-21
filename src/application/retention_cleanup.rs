use crate::domain::retention::RetentionPolicy;
use crate::infrastructure::sql_store::SqlExecutor;
use crate::infrastructure::workspace::MeetingWorkspaceLayout;
use std::fs;
use std::io;
use std::path::Path;

// Raw-audio and transcript workspace queries share the same filters today, but
// stay separate so their retention scopes can diverge without changing callers.
pub const RETENTION_EXPIRED_RAW_WORKSPACES_SQL: &str = r#"
SELECT id, guild_id, voice_channel_id
FROM meetings
WHERE stopped_at IS NOT NULL
  AND stopped_at < NOW() - (($1 || ' days')::interval)
  AND status IN ('posted', 'failed', 'aborted')
"#;

// See RETENTION_EXPIRED_RAW_WORKSPACES_SQL for why this intentionally mirrors
// the raw-audio workspace query instead of aliasing it.
pub const RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL: &str = r#"
SELECT DISTINCT m.id, m.guild_id, m.voice_channel_id
FROM meetings m
JOIN transcripts t ON t.meeting_id = m.id
WHERE t.is_deleted = FALSE
  AND t.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

// Summary workspace cleanup is gated by RETENTION_SUMMARY_TTL_DAYS and uses
// its own constant so future summary-specific filters can diverge.
pub const RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL: &str = r#"
SELECT DISTINCT m.id, m.guild_id, m.voice_channel_id
FROM meetings m
JOIN summaries s ON s.meeting_id = m.id
WHERE s.created_at < NOW() - (($1 || ' days')::interval)
  AND m.status IN ('posted', 'failed', 'aborted')
"#;

pub const RETENTION_MARK_TRANSCRIPTS_DELETED_SQL: &str = r#"
UPDATE transcripts
SET is_deleted=TRUE
WHERE is_deleted=FALSE
  AND created_at < NOW() - (($1 || ' days')::interval)
"#;

pub const RETENTION_DELETE_SUMMARIES_SQL: &str = r#"
DELETE FROM summaries
WHERE created_at < NOW() - (($1 || ' days')::interval)
"#;

pub const RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts
WHERE expires_at IS NOT NULL
  AND expires_at <= NOW()
"#;

pub const RETENTION_DELETE_RAW_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts
WHERE kind IN ('raw_audio', 'audio', 'mixdown_audio', 'speaker_audio')
  AND created_at < NOW() - (($1 || ' days')::interval)
"#;

pub const RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts
WHERE kind IN ('transcript', 'masked_transcript')
  AND created_at < NOW() - (($1 || ' days')::interval)
"#;

pub const RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts
WHERE kind IN ('summary', 'summary_markdown')
  AND created_at < NOW() - (($1 || ' days')::interval)
"#;

pub const RETENTION_DELETE_DEBUG_ARTIFACTS_SQL: &str = r#"
DELETE FROM artifacts
WHERE kind IN ('debug', 'debug_artifact', 'whisper_debug')
  AND created_at < NOW() - (($1 || ' days')::interval)
"#;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionCleanupReport {
    pub raw_workspaces_scanned: usize,
    pub raw_audio_dirs_removed: usize,
    pub legacy_raw_audio_removed: usize,
    pub speaker_dirs_removed: usize,
    pub context_dirs_removed: usize,
    pub transcript_dirs_removed: usize,
    pub summary_dirs_removed: usize,
    pub debug_dirs_removed: usize,
    pub transcripts_marked_deleted: u64,
    pub summaries_deleted: u64,
    pub artifacts_deleted: u64,
}

impl RetentionCleanupReport {
    pub fn merge(&mut self, other: RetentionCleanupReport) {
        self.raw_workspaces_scanned += other.raw_workspaces_scanned;
        self.raw_audio_dirs_removed += other.raw_audio_dirs_removed;
        self.legacy_raw_audio_removed += other.legacy_raw_audio_removed;
        self.speaker_dirs_removed += other.speaker_dirs_removed;
        self.context_dirs_removed += other.context_dirs_removed;
        self.transcript_dirs_removed += other.transcript_dirs_removed;
        self.summary_dirs_removed += other.summary_dirs_removed;
        self.debug_dirs_removed += other.debug_dirs_removed;
        self.transcripts_marked_deleted += other.transcripts_marked_deleted;
        self.summaries_deleted += other.summaries_deleted;
        self.artifacts_deleted += other.artifacts_deleted;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionCleanupPlan {
    pub raw_workspaces: Vec<ExpiredWorkspaceRow>,
    pub transcript_workspaces: Vec<ExpiredWorkspaceRow>,
    pub summary_workspaces: Vec<ExpiredWorkspaceRow>,
}

pub fn enforce_retention_policy<E: SqlExecutor>(
    executor: &mut E,
    workspace_layout: &MeetingWorkspaceLayout,
    policy: RetentionPolicy,
) -> Result<RetentionCleanupReport, String> {
    let plan = collect_retention_cleanup_plan(executor, policy)?;
    let mut report = RetentionCleanupReport::default();
    let filesystem_result = apply_retention_filesystem_cleanup(workspace_layout, &plan);
    report.merge(apply_retention_database_cleanup(executor, policy)?);
    report.merge(filesystem_result?);
    Ok(report)
}

pub fn collect_retention_cleanup_plan<E: SqlExecutor>(
    executor: &mut E,
    policy: RetentionPolicy,
) -> Result<RetentionCleanupPlan, String> {
    let raw_ttl = policy.raw_audio_ttl_days.to_string();
    let transcript_ttl = policy.transcript_ttl_days.to_string();

    let expired_raw_workspaces = executor.query_rows(
        RETENTION_EXPIRED_RAW_WORKSPACES_SQL,
        std::slice::from_ref(&raw_ttl),
    )?;
    let raw_workspaces = expired_raw_workspaces
        .into_iter()
        .map(|row| parse_workspace_row(&row))
        .collect::<Result<Vec<_>, _>>()?;

    let expired_transcript_workspaces = executor.query_rows(
        RETENTION_EXPIRED_TRANSCRIPT_WORKSPACES_SQL,
        std::slice::from_ref(&transcript_ttl),
    )?;
    let transcript_workspaces = expired_transcript_workspaces
        .into_iter()
        .map(|row| parse_workspace_row(&row))
        .collect::<Result<Vec<_>, _>>()?;

    let summary_workspaces = if let Some(summary_ttl_days) = policy.summary_ttl_days {
        let summary_ttl = summary_ttl_days.to_string();
        executor
            .query_rows(
                RETENTION_EXPIRED_SUMMARY_WORKSPACES_SQL,
                std::slice::from_ref(&summary_ttl),
            )?
            .into_iter()
            .map(|row| parse_workspace_row(&row))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    Ok(RetentionCleanupPlan {
        raw_workspaces,
        transcript_workspaces,
        summary_workspaces,
    })
}

pub fn apply_retention_filesystem_cleanup(
    workspace_layout: &MeetingWorkspaceLayout,
    plan: &RetentionCleanupPlan,
) -> Result<RetentionCleanupReport, String> {
    let mut report = RetentionCleanupReport::default();
    for meeting in &plan.raw_workspaces {
        report.raw_workspaces_scanned += 1;
        let workspace = workspace_layout.for_meeting(
            &meeting.guild_id,
            &meeting.voice_channel_id,
            &meeting.meeting_id,
        );
        if remove_dir_if_present(&workspace.audio_dir())? {
            report.raw_audio_dirs_removed += 1;
        }
        if remove_legacy_raw_audio(&workspace_layout.legacy_meeting_dir(&meeting.meeting_id))? {
            report.legacy_raw_audio_removed += 1;
        }
        if remove_dir_if_present(&workspace.speakers_dir())? {
            report.speaker_dirs_removed += 1;
        }
        if remove_dir_if_present(&workspace.context_dir())? {
            report.context_dirs_removed += 1;
        }
        if remove_dir_if_present(&workspace.debug_dir())? {
            report.debug_dirs_removed += 1;
        }
    }

    for meeting in &plan.transcript_workspaces {
        let workspace = workspace_layout.for_meeting(
            &meeting.guild_id,
            &meeting.voice_channel_id,
            &meeting.meeting_id,
        );
        if remove_dir_if_present(&workspace.transcript_dir())? {
            report.transcript_dirs_removed += 1;
        }
    }

    for meeting in &plan.summary_workspaces {
        let workspace = workspace_layout.for_meeting(
            &meeting.guild_id,
            &meeting.voice_channel_id,
            &meeting.meeting_id,
        );
        if remove_dir_if_present(&workspace.summary_dir())? {
            report.summary_dirs_removed += 1;
        }
    }
    Ok(report)
}

pub fn apply_retention_database_cleanup<E: SqlExecutor>(
    executor: &mut E,
    policy: RetentionPolicy,
) -> Result<RetentionCleanupReport, String> {
    let mut report = RetentionCleanupReport::default();
    let raw_ttl = policy.raw_audio_ttl_days.to_string();
    let transcript_ttl = policy.transcript_ttl_days.to_string();

    report.transcripts_marked_deleted += executor.execute(
        RETENTION_MARK_TRANSCRIPTS_DELETED_SQL,
        std::slice::from_ref(&transcript_ttl),
    )?;
    report.artifacts_deleted += executor.execute(RETENTION_DELETE_EXPIRED_ARTIFACTS_SQL, &[])?;
    report.artifacts_deleted += executor.execute(
        RETENTION_DELETE_RAW_ARTIFACTS_SQL,
        std::slice::from_ref(&raw_ttl),
    )?;
    report.artifacts_deleted += executor.execute(
        RETENTION_DELETE_TRANSCRIPT_ARTIFACTS_SQL,
        std::slice::from_ref(&transcript_ttl),
    )?;
    // Debug artifacts intentionally share the raw-audio TTL; add a
    // dedicated RETENTION_DEBUG_TTL_DAYS if independent control is needed.
    report.artifacts_deleted += executor.execute(
        RETENTION_DELETE_DEBUG_ARTIFACTS_SQL,
        std::slice::from_ref(&raw_ttl),
    )?;

    if let Some(summary_ttl_days) = policy.summary_ttl_days {
        let summary_ttl = summary_ttl_days.to_string();
        report.summaries_deleted += executor.execute(
            RETENTION_DELETE_SUMMARIES_SQL,
            std::slice::from_ref(&summary_ttl),
        )?;
        report.artifacts_deleted += executor.execute(
            RETENTION_DELETE_SUMMARY_ARTIFACTS_SQL,
            std::slice::from_ref(&summary_ttl),
        )?;
    }

    Ok(report)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredWorkspaceRow {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
}

fn parse_workspace_row(row: &[String]) -> Result<ExpiredWorkspaceRow, String> {
    if row.len() < 3 {
        return Err(format!(
            "invalid retention workspace row length: {}",
            row.len()
        ));
    }
    Ok(ExpiredWorkspaceRow {
        meeting_id: row[0].clone(),
        guild_id: row[1].clone(),
        voice_channel_id: row[2].clone(),
    })
}

fn remove_dir_if_present(path: &Path) -> Result<bool, String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("failed to remove {}: {err}", path.display())),
    }
}

fn remove_legacy_raw_audio(meeting_dir: &Path) -> Result<bool, String> {
    let mut removed = false;
    match fs::read_dir(meeting_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry
                    .map_err(|err| format!("failed to read {}: {err}", meeting_dir.display()))?;
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("wav") {
                    fs::remove_file(&path)
                        .map_err(|err| format!("failed to remove {}: {err}", path.display()))?;
                    removed = true;
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(format!("failed to read {}: {err}", meeting_dir.display())),
    }

    if remove_dir_if_present(&meeting_dir.join("speakers"))? {
        removed = true;
    }
    remove_empty_dir_if_present(meeting_dir)?;
    Ok(removed)
}

fn remove_empty_dir_if_present(path: &Path) -> Result<bool, String> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
        Err(err) => Err(format!("failed to remove empty {}: {err}", path.display())),
    }
}
