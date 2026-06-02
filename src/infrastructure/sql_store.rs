use crate::domain::MeetingStatus;
use crate::domain::StopReason;
use crate::domain::{JobStatus, JobType};
use crate::infrastructure::queue::{Job, JobQueue, QueueError};
use crate::infrastructure::sql::{
    BACKFILL_DEFAULT_TENANTS_FROM_EXISTING_GUILDS_SQL, CLAIM_JOB_BY_ID_SQL, CLAIM_JOB_SQL,
    ENQUEUE_JOB_SQL, GET_EFFECTIVE_MEETING_SETTINGS_SQL,
    GET_GUILD_SETTINGS_FOR_MEETING_SNAPSHOT_SQL,
    INSERT_RECORDING_MEETING_WITH_EFFECTIVE_SETTINGS_SQL,
    INSERT_SCHEDULED_MEETING_WITH_EFFECTIVE_SETTINGS_SQL, MARK_JOB_DONE_SQL, MARK_JOB_FAILED_SQL,
    MARK_STOPPING_IF_RECORDING_SQL, RESOLVE_TENANT_BY_GUILD_SQL, RETRY_JOB_SQL,
    SET_MEETING_STATUS_CAS_SQL, UPSERT_EFFECTIVE_MEETING_SETTINGS_SQL,
};
use crate::infrastructure::storage::{
    CreateMeetingRequest, EffectiveMeetingSettings, GuildSettingsForSnapshot, MeetingStore,
    StatusMessageMetadata, StopTransition, StoreError, StoredMeeting,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use tokio_postgres::{Client as PgClient, NoTls, Row};

/// Prefix added to error messages when PostgreSQL returns SQLSTATE 23505
/// (unique_violation). Callers can check `err.starts_with(UNIQUE_VIOLATION_PREFIX)`
/// instead of locale-dependent string matching.
pub const UNIQUE_VIOLATION_PREFIX: &str = "UNIQUE_VIOLATION: ";

pub type SqlRow = Vec<Option<String>>;

pub trait SqlExecutor {
    fn execute(&mut self, sql: &str, params: &[String]) -> Result<u64, String>;
    fn query_active_meeting(&mut self, guild_id: &str) -> Result<Option<StoredMeeting>, String>;
    fn query_rows(&mut self, sql: &str, params: &[String]) -> Result<Vec<SqlRow>, String>;
    fn run_migration(&mut self, migration_sql: &str) -> Result<(), String>;
}

/// Test helper: map plain strings to `Some` columns (SQL non-NULL values).
pub fn sql_row_from_strings(values: Vec<String>) -> SqlRow {
    values.into_iter().map(Some).collect()
}

#[derive(Debug, Default)]
pub struct FakeSqlExecutor {
    pub executed: Vec<(String, Vec<String>)>,
    pub active_by_guild: HashMap<String, StoredMeeting>,
    pub query_rows_result: HashMap<String, Vec<SqlRow>>,
    pub query_rows_error: HashMap<String, String>,
    pub execute_result: HashMap<String, u64>,
    pub execute_error: HashMap<String, String>,
}

impl SqlExecutor for FakeSqlExecutor {
    fn execute(&mut self, sql: &str, params: &[String]) -> Result<u64, String> {
        self.executed.push((sql.to_owned(), params.to_vec()));
        let key = format!("{}|{}", sql, params.join("\u{1f}"));
        if let Some(err) = self.execute_error.get(&key) {
            return Err(err.clone());
        }
        Ok(*self.execute_result.get(&key).unwrap_or(&1))
    }

    fn query_active_meeting(&mut self, guild_id: &str) -> Result<Option<StoredMeeting>, String> {
        Ok(self.active_by_guild.get(guild_id).cloned())
    }

    fn query_rows(&mut self, sql: &str, params: &[String]) -> Result<Vec<SqlRow>, String> {
        self.executed.push((sql.to_owned(), params.to_vec()));
        let key = format!("{}|{}", sql, params.join("\u{1f}"));
        if let Some(err) = self.query_rows_error.get(&key) {
            return Err(err.clone());
        }
        Ok(self
            .query_rows_result
            .get(&key)
            .cloned()
            .unwrap_or_default())
    }

    fn run_migration(&mut self, migration_sql: &str) -> Result<(), String> {
        self.executed.push((migration_sql.to_owned(), Vec::new()));
        Ok(())
    }
}

pub struct SqlMeetingStore<E: SqlExecutor> {
    pub executor: E,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTenantInstallation {
    pub tenant_id: String,
    pub tenant_status: String,
    pub period_anchor: Option<DateTime<Utc>>,
    pub guild_id: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultTenantBackfill {
    pub tenants_inserted: u64,
    pub installations_inserted: u64,
}

impl<E: SqlExecutor> SqlMeetingStore<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }

    pub fn apply_initial_migration(&mut self, migration_sql: &str) -> Result<(), String> {
        self.executor.run_migration(migration_sql)
    }

    pub fn resolve_tenant_by_guild(
        &mut self,
        guild_id: &str,
    ) -> Result<Option<ResolvedTenantInstallation>, StoreError> {
        let rows = self
            .executor
            .query_rows(RESOLVE_TENANT_BY_GUILD_SQL, &[guild_id.to_owned()])
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_resolved_tenant_installation_row(&row).map(Some)
    }

    pub fn backfill_default_tenants_from_existing_guilds(
        &mut self,
    ) -> Result<DefaultTenantBackfill, StoreError> {
        let rows = self
            .executor
            .query_rows(BACKFILL_DEFAULT_TENANTS_FROM_EXISTING_GUILDS_SQL, &[])
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(StoreError::Backend(
                "default tenant backfill returned no counts".to_owned(),
            ));
        };
        parse_default_tenant_backfill_row(&row)
    }

    pub fn get_guild_settings_for_meeting_snapshot(
        &mut self,
        guild_id: &str,
    ) -> Result<Option<GuildSettingsForSnapshot>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                GET_GUILD_SETTINGS_FOR_MEETING_SNAPSHOT_SQL,
                &[guild_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_guild_settings_for_snapshot_row(&row).map(Some)
    }
}

pub struct SqlJobQueue<E: SqlExecutor> {
    pub executor: E,
}

impl<E: SqlExecutor> SqlJobQueue<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }
}

impl<E: SqlExecutor> JobQueue for SqlJobQueue<E> {
    fn enqueue(&mut self, job: Job) -> Result<(), QueueError> {
        let job_id = job.id.clone();
        let result = self.executor.execute(
            ENQUEUE_JOB_SQL,
            &[job.id, job.meeting_id, job.job_type.as_str().to_owned()],
        );
        match result {
            Ok(_) => {}
            Err(err) => {
                if err.starts_with(UNIQUE_VIOLATION_PREFIX) {
                    return Err(QueueError::AlreadyExists { job_id });
                }
                return Err(QueueError::Backend(err));
            }
        }
        Ok(())
    }

    fn claim_next(&mut self, job_type: JobType) -> Result<Option<Job>, QueueError> {
        let rows = self
            .executor
            .query_rows(CLAIM_JOB_SQL, &[job_type.as_str().to_owned()])
            .map_err(QueueError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_job_row(&row).map(Some)
    }

    fn claim_by_id(&mut self, job_id: &str) -> Result<Option<Job>, QueueError> {
        let rows = self
            .executor
            .query_rows(CLAIM_JOB_BY_ID_SQL, &[job_id.to_owned()])
            .map_err(QueueError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_job_row(&row).map(Some)
    }

    fn mark_done(&mut self, job_id: &str) -> Result<(), QueueError> {
        // SQL has `AND status = 'running'`, so affected==0 can mean either
        // "job not found" or "job exists but not in running state". We return
        // NotFound because SQL cannot distinguish the two without a second query.
        let affected = self
            .executor
            .execute(MARK_JOB_DONE_SQL, &[job_id.to_owned()])
            .map_err(QueueError::Backend)?;
        if affected == 0 {
            return Err(QueueError::NotFound {
                job_id: job_id.to_owned(),
            });
        }
        Ok(())
    }

    fn mark_failed(&mut self, job_id: &str, error_message: String) -> Result<(), QueueError> {
        let affected = self
            .executor
            .execute(MARK_JOB_FAILED_SQL, &[job_id.to_owned(), error_message])
            .map_err(QueueError::Backend)?;
        if affected == 0 {
            return Err(QueueError::NotFound {
                job_id: job_id.to_owned(),
            });
        }
        Ok(())
    }

    fn retry(
        &mut self,
        job_id: &str,
        error_message: String,
        max_retries: u32,
    ) -> Result<JobStatus, QueueError> {
        let rows = self
            .executor
            .query_rows(
                RETRY_JOB_SQL,
                &[job_id.to_owned(), error_message, max_retries.to_string()],
            )
            .map_err(QueueError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(QueueError::NotFound {
                job_id: job_id.to_owned(),
            });
        };
        let status_value = row
            .first()
            .and_then(|v| v.clone())
            .ok_or_else(|| QueueError::Backend("retry returned no status".to_owned()))?;
        JobStatus::parse_str(&status_value).ok_or_else(|| {
            QueueError::Backend(format!(
                "unknown job status in retry result: {status_value}"
            ))
        })
    }
}

fn require_job_column(
    row: &[Option<String>],
    idx: usize,
    field: &str,
) -> Result<String, QueueError> {
    row.get(idx)
        .and_then(|v| v.clone())
        .ok_or_else(|| QueueError::Backend(format!("{field} is NULL")))
}

fn parse_job_row(row: &SqlRow) -> Result<Job, QueueError> {
    if row.len() < 6 {
        return Err(QueueError::Backend(format!(
            "invalid claimed job row length: {}",
            row.len()
        )));
    }
    let job_type_raw = require_job_column(row, 2, "job_type")?;
    let job_type = JobType::parse_str(&job_type_raw)
        .ok_or_else(|| QueueError::Backend(format!("unknown job type: {job_type_raw}")))?;
    let status_raw = require_job_column(row, 3, "status")?;
    let status = JobStatus::parse_str(&status_raw)
        .ok_or_else(|| QueueError::Backend(format!("unknown job status: {status_raw}")))?;
    let retry_count_raw = require_job_column(row, 4, "retry_count")?;
    let retry_count = retry_count_raw.parse::<u32>().map_err(|err| {
        QueueError::Backend(format!("invalid retry_count '{retry_count_raw}': {err}"))
    })?;

    Ok(Job {
        id: require_job_column(row, 0, "id")?,
        meeting_id: require_job_column(row, 1, "meeting_id")?,
        job_type,
        status,
        retry_count,
        error_message: row.get(5).and_then(|v| v.clone()),
    })
}

fn require_store_column(
    row: &[Option<String>],
    idx: usize,
    field: &str,
) -> Result<String, StoreError> {
    row.get(idx)
        .and_then(|v| v.clone())
        .ok_or_else(|| StoreError::Backend(format!("{field} is NULL")))
}

fn parse_resolved_tenant_installation_row(
    row: &SqlRow,
) -> Result<ResolvedTenantInstallation, StoreError> {
    if row.len() < 5 {
        return Err(StoreError::Backend(format!(
            "invalid tenant installation row length: {}",
            row.len()
        )));
    }
    Ok(ResolvedTenantInstallation {
        tenant_id: require_store_column(row, 0, "tenant_id")?,
        tenant_status: require_store_column(row, 1, "tenant_status")?,
        period_anchor: parse_optional_tenant_period_anchor(row.get(2).and_then(|v| v.clone()))?,
        guild_id: require_store_column(row, 3, "guild_id")?,
        source: require_store_column(row, 4, "source")?,
    })
}

fn parse_default_tenant_backfill_row(row: &SqlRow) -> Result<DefaultTenantBackfill, StoreError> {
    if row.len() < 2 {
        return Err(StoreError::Backend(format!(
            "invalid default tenant backfill row length: {}",
            row.len()
        )));
    }
    let tenants_inserted = require_store_column(row, 0, "tenants_inserted")?;
    let installations_inserted = require_store_column(row, 1, "installations_inserted")?;

    Ok(DefaultTenantBackfill {
        tenants_inserted: tenants_inserted.parse::<u64>().map_err(|err| {
            StoreError::Backend(format!(
                "invalid tenants_inserted count '{tenants_inserted}': {err}"
            ))
        })?,
        installations_inserted: installations_inserted.parse::<u64>().map_err(|err| {
            StoreError::Backend(format!(
                "invalid installations_inserted count '{installations_inserted}': {err}"
            ))
        })?,
    })
}

fn optional_u64_column(row: &SqlRow, idx: usize, field: &str) -> Result<Option<u64>, StoreError> {
    let Some(raw) = row.get(idx).and_then(|v| v.clone()) else {
        return Ok(None);
    };
    raw.parse::<u64>()
        .map(Some)
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn optional_u32_column(row: &SqlRow, idx: usize, field: &str) -> Result<Option<u32>, StoreError> {
    let Some(raw) = row.get(idx).and_then(|v| v.clone()) else {
        return Ok(None);
    };
    raw.parse::<u32>()
        .map(Some)
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn required_u64_column(row: &SqlRow, idx: usize, field: &str) -> Result<u64, StoreError> {
    let raw = require_store_column(row, idx, field)?;
    raw.parse::<u64>()
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn required_u32_column(row: &SqlRow, idx: usize, field: &str) -> Result<u32, StoreError> {
    let raw = require_store_column(row, idx, field)?;
    raw.parse::<u32>()
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn required_f32_column(row: &SqlRow, idx: usize, field: &str) -> Result<f32, StoreError> {
    let raw = require_store_column(row, idx, field)?;
    let parsed = raw
        .parse::<f32>()
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))?;
    if !parsed.is_finite() {
        return Err(StoreError::Backend(format!(
            "invalid {field} '{raw}': must be finite"
        )));
    }
    Ok(parsed)
}

fn optional_bool_column(row: &SqlRow, idx: usize, field: &str) -> Result<Option<bool>, StoreError> {
    let Some(raw) = row.get(idx).and_then(|v| v.clone()) else {
        return Ok(None);
    };
    raw.parse::<bool>()
        .map(Some)
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn required_bool_column(row: &SqlRow, idx: usize, field: &str) -> Result<bool, StoreError> {
    let raw = require_store_column(row, idx, field)?;
    raw.parse::<bool>()
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn parse_guild_settings_for_snapshot_row(
    row: &SqlRow,
) -> Result<GuildSettingsForSnapshot, StoreError> {
    if row.len() < 7 {
        return Err(StoreError::Backend(format!(
            "invalid guild settings snapshot row length: {}",
            row.len()
        )));
    }
    Ok(GuildSettingsForSnapshot {
        whisper_language: row.first().and_then(|v| v.clone()),
        whisper_language_explicit: required_bool_column(row, 1, "whisper_language_explicit")?,
        whisper_vad: optional_bool_column(row, 2, "whisper_vad")?,
        auto_stop_grace_seconds: optional_u64_column(row, 3, "auto_stop_grace_seconds")?,
        retention_raw_audio_ttl_days: optional_u32_column(row, 4, "retention_raw_audio_ttl_days")?,
        retention_transcript_ttl_days: optional_u32_column(
            row,
            5,
            "retention_transcript_ttl_days",
        )?,
        summary_enabled: optional_bool_column(row, 6, "summary_enabled")?,
    })
}

fn parse_effective_meeting_settings_row(
    row: &SqlRow,
) -> Result<EffectiveMeetingSettings, StoreError> {
    if row.len() < 14 {
        return Err(StoreError::Backend(format!(
            "invalid effective meeting settings row length: {}",
            row.len()
        )));
    }
    Ok(EffectiveMeetingSettings {
        whisper_language: row.first().and_then(|v| v.clone()),
        whisper_vad: required_bool_column(row, 1, "whisper_vad")?,
        whisper_beam_size: required_u32_column(row, 2, "whisper_beam_size")?,
        whisper_suppress_non_speech: required_bool_column(row, 3, "whisper_suppress_non_speech")?,
        whisper_prompt: row.get(4).and_then(|v| v.clone()),
        whisper_temperature: required_f32_column(row, 5, "whisper_temperature")?,
        whisper_resample_to_16k: required_bool_column(row, 6, "whisper_resample_to_16k")?,
        auto_stop_grace_seconds: required_u64_column(row, 7, "auto_stop_grace_seconds")?,
        retention_raw_audio_ttl_days: required_u32_column(row, 8, "retention_raw_audio_ttl_days")?,
        retention_transcript_ttl_days: required_u32_column(
            row,
            9,
            "retention_transcript_ttl_days",
        )?,
        retention_summary_ttl_days: optional_u32_column(row, 10, "retention_summary_ttl_days")?,
        summary_enabled: required_bool_column(row, 11, "summary_enabled")?,
        summary_template_id: row.get(12).and_then(|v| v.clone()),
        domain_knowledge_version_id: row.get(13).and_then(|v| v.clone()),
    })
}

fn parse_optional_tenant_period_anchor(
    value: Option<String>,
) -> Result<Option<DateTime<Utc>>, StoreError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(&raw)
        .map(|ts| Some(ts.with_timezone(&Utc)))
        .map_err(|err| StoreError::Backend(format!("invalid tenant period_anchor '{raw}': {err}")))
}

impl<E: SqlExecutor> MeetingStore for SqlMeetingStore<E> {
    fn mark_stopping_if_recording(
        &mut self,
        meeting_id: &str,
        reason: StopReason,
    ) -> Result<StopTransition, StoreError> {
        let sql = MARK_STOPPING_IF_RECORDING_SQL;
        let affected = self
            .executor
            .execute(sql, &[reason.as_str().to_owned(), meeting_id.to_owned()])
            .map_err(StoreError::Backend)?;
        if affected == 1 {
            Ok(StopTransition::Acquired)
        } else {
            Ok(StopTransition::AlreadyStoppingOrStopped)
        }
    }

    fn find_active_meeting_by_guild(
        &mut self,
        guild_id: &str,
    ) -> Result<Option<StoredMeeting>, StoreError> {
        self.executor
            .query_active_meeting(guild_id)
            .map_err(StoreError::Backend)
    }

    fn get_meeting(&mut self, meeting_id: &str) -> Result<Option<StoredMeeting>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                "SELECT id, guild_id, voice_channel_id, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, \
                        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
                        to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as stopped_at \
                  FROM meetings WHERE id=$1 LIMIT 1",
                &[meeting_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        if row.len() < 13 {
            return Err(StoreError::Backend(format!(
                "invalid meeting row length for id={meeting_id}: {}",
                row.len()
            )));
        }
        let require = |idx: usize, field: &str| -> Result<String, StoreError> {
            row.get(idx)
                .and_then(|v| v.clone())
                .ok_or_else(|| StoreError::Backend(format!("{field} is NULL for id={meeting_id}")))
        };
        let status_raw = require(8, "status")?;
        let status = MeetingStatus::parse_str(&status_raw).ok_or_else(|| {
            StoreError::Backend(format!(
                "invalid meeting status for id={meeting_id}: {status_raw}"
            ))
        })?;
        let stop_reason = parse_stop_reason_column(
            row.get(9).and_then(|v| v.clone()),
            &format!("meeting_id={meeting_id}"),
        )
        .map_err(StoreError::Backend)?;
        Ok(Some(StoredMeeting {
            id: require(0, "id")?,
            guild_id: require(1, "guild_id")?,
            voice_channel_id: require(2, "voice_channel_id")?,
            report_channel_id: require(3, "report_channel_id")?,
            status_message_channel_id: row.get(4).and_then(|v| v.clone()),
            status_message_id: row.get(5).and_then(|v| v.clone()),
            started_by_user_id: require(6, "started_by_user_id")?,
            title: row.get(7).and_then(|v| v.clone()),
            status,
            stop_reason,
            error_message: row.get(10).and_then(|v| v.clone()),
            started_at: parse_optional_rfc3339(row.get(11).and_then(|v| v.clone())),
            stopped_at: parse_optional_rfc3339(row.get(12).and_then(|v| v.clone())),
        }))
    }

    fn create_scheduled_meeting(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError> {
        let meeting_id = request.id.clone();
        if let Some(settings) = request.effective_settings.clone() {
            let params = create_meeting_with_settings_params(request, settings);
            self.executor
                .execute(
                    INSERT_SCHEDULED_MEETING_WITH_EFFECTIVE_SETTINGS_SQL,
                    &params,
                )
                .map_err(|err| {
                    if err.starts_with(UNIQUE_VIOLATION_PREFIX) {
                        StoreError::AlreadyExists { meeting_id }
                    } else {
                        StoreError::Backend(err)
                    }
                })?;
        } else {
            let sql = "INSERT INTO meetings(id,guild_id,voice_channel_id,report_channel_id,status_message_channel_id,status_message_id,started_by_user_id,status) VALUES($1,$2,$3,$4,NULLIF($5,''),NULLIF($6,''),$7,'scheduled')";
            self.executor
                .execute(
                    sql,
                    &[
                        request.id,
                        request.guild_id,
                        request.voice_channel_id,
                        request.report_channel_id,
                        request.status_message_channel_id.unwrap_or_default(),
                        request.status_message_id.unwrap_or_default(),
                        request.started_by_user_id,
                    ],
                )
                .map_err(|err| {
                    if err.starts_with(UNIQUE_VIOLATION_PREFIX) {
                        StoreError::AlreadyExists { meeting_id }
                    } else {
                        StoreError::Backend(err)
                    }
                })?;
        }
        Ok(())
    }

    fn create_meeting_as_recording(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError> {
        let meeting_id = request.id.clone();
        if let Some(settings) = request.effective_settings.clone() {
            let params = create_meeting_with_settings_params(request, settings);
            self.executor
                .execute(
                    INSERT_RECORDING_MEETING_WITH_EFFECTIVE_SETTINGS_SQL,
                    &params,
                )
                .map_err(|err| {
                    if err.starts_with(UNIQUE_VIOLATION_PREFIX) {
                        StoreError::AlreadyExists { meeting_id }
                    } else {
                        StoreError::Backend(err)
                    }
                })?;
        } else {
            let sql = "INSERT INTO meetings(id,guild_id,voice_channel_id,report_channel_id,status_message_channel_id,status_message_id,started_by_user_id,status) VALUES($1,$2,$3,$4,NULLIF($5,''),NULLIF($6,''),$7,'recording')";
            self.executor
                .execute(
                    sql,
                    &[
                        request.id,
                        request.guild_id,
                        request.voice_channel_id,
                        request.report_channel_id,
                        request.status_message_channel_id.unwrap_or_default(),
                        request.status_message_id.unwrap_or_default(),
                        request.started_by_user_id,
                    ],
                )
                .map_err(|err| {
                    if err.starts_with(UNIQUE_VIOLATION_PREFIX) {
                        StoreError::AlreadyExists { meeting_id }
                    } else {
                        StoreError::Backend(err)
                    }
                })?;
        }
        Ok(())
    }

    fn set_meeting_status(
        &mut self,
        meeting_id: &str,
        status: MeetingStatus,
        expected_current: Option<MeetingStatus>,
    ) -> Result<(), StoreError> {
        let status_value = status.as_str();
        match expected_current {
            Some(expected) => {
                let rows = self
                    .executor
                    .query_rows(
                        SET_MEETING_STATUS_CAS_SQL,
                        &[
                            status_value.to_owned(),
                            meeting_id.to_owned(),
                            expected.as_str().to_owned(),
                        ],
                    )
                    .map_err(StoreError::Backend)?;
                let outcome = rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(|value| value.as_deref())
                    .ok_or_else(|| {
                        StoreError::Backend(
                            "set_meeting_status CAS query returned no outcome".to_owned(),
                        )
                    })?;
                match outcome {
                    "updated" => Ok(()),
                    "conflict" => Err(StoreError::CasConflict {
                        meeting_id: meeting_id.to_owned(),
                    }),
                    "not_found" => Err(StoreError::NotFound {
                        meeting_id: meeting_id.to_owned(),
                    }),
                    _ => Err(StoreError::Backend(format!(
                        "set_meeting_status CAS query returned unknown outcome: {outcome}"
                    ))),
                }
            }
            None => {
                let affected = self
                    .executor
                    .execute(
                        "UPDATE meetings SET status=$1, updated_at=NOW() WHERE id=$2",
                        &[status_value.to_owned(), meeting_id.to_owned()],
                    )
                    .map_err(StoreError::Backend)?;
                if affected == 0 {
                    return Err(StoreError::NotFound {
                        meeting_id: meeting_id.to_owned(),
                    });
                }
                Ok(())
            }
        }
    }

    fn set_error_message(
        &mut self,
        meeting_id: &str,
        error_message: Option<String>,
    ) -> Result<(), StoreError> {
        let affected = self
            .executor
            .execute(
                "UPDATE meetings SET error_message=NULLIF($1, ''), updated_at=NOW() WHERE id=$2",
                &[error_message.unwrap_or_default(), meeting_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        if affected == 0 {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        }
        Ok(())
    }

    fn get_status_message_metadata(
        &mut self,
        meeting_id: &str,
    ) -> Result<StatusMessageMetadata, StoreError> {
        let rows = self
            .executor
            .query_rows(
                "SELECT report_channel_id, status_message_channel_id, status_message_id FROM meetings WHERE id=$1 LIMIT 1",
                &[meeting_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        };

        let report_channel_id = row.first().and_then(|v| v.clone()).ok_or_else(|| {
            StoreError::Backend(format!(
                "report_channel_id missing in status metadata row for meeting_id={meeting_id}"
            ))
        })?;
        let status_message_channel_id = row.get(1).and_then(|v| v.clone());
        let status_message_id = row.get(2).and_then(|v| v.clone());

        Ok(StatusMessageMetadata {
            report_channel_id,
            status_message_channel_id,
            status_message_id,
        })
    }

    fn set_status_message(
        &mut self,
        meeting_id: &str,
        channel_id: String,
        message_id: String,
    ) -> Result<(), StoreError> {
        let affected = self
            .executor
            .execute(
                "UPDATE meetings SET status_message_channel_id=$1, status_message_id=$2, updated_at=NOW() WHERE id=$3",
                &[channel_id, message_id, meeting_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        if affected == 0 {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        }
        Ok(())
    }

    fn upsert_effective_meeting_settings(
        &mut self,
        meeting_id: &str,
        settings: EffectiveMeetingSettings,
    ) -> Result<(), StoreError> {
        let affected = self
            .executor
            .execute(
                UPSERT_EFFECTIVE_MEETING_SETTINGS_SQL,
                &effective_settings_params(meeting_id, &settings),
            )
            .map_err(StoreError::Backend)?;
        if affected == 0 {
            return Err(StoreError::NotFound {
                meeting_id: meeting_id.to_owned(),
            });
        }
        Ok(())
    }

    fn get_effective_meeting_settings(
        &mut self,
        meeting_id: &str,
    ) -> Result<Option<EffectiveMeetingSettings>, StoreError> {
        let rows = self
            .executor
            .query_rows(GET_EFFECTIVE_MEETING_SETTINGS_SQL, &[meeting_id.to_owned()])
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_effective_meeting_settings_row(&row).map(Some)
    }
}

fn create_meeting_with_settings_params(
    request: CreateMeetingRequest,
    settings: EffectiveMeetingSettings,
) -> Vec<String> {
    let mut params = vec![
        request.id,
        request.guild_id,
        request.voice_channel_id,
        request.report_channel_id,
        request.status_message_channel_id.unwrap_or_default(),
        request.status_message_id.unwrap_or_default(),
        request.started_by_user_id,
    ];
    params.extend(effective_settings_values(&settings));
    params
}

fn effective_settings_params(meeting_id: &str, settings: &EffectiveMeetingSettings) -> Vec<String> {
    let mut params = vec![meeting_id.to_owned()];
    params.extend(effective_settings_values(settings));
    params
}

fn effective_settings_values(settings: &EffectiveMeetingSettings) -> Vec<String> {
    vec![
        settings.whisper_language.clone().unwrap_or_default(),
        settings.whisper_vad.to_string(),
        settings.whisper_beam_size.to_string(),
        settings.whisper_suppress_non_speech.to_string(),
        settings.whisper_prompt.clone().unwrap_or_default(),
        settings.whisper_temperature.to_string(),
        settings.whisper_resample_to_16k.to_string(),
        settings.auto_stop_grace_seconds.to_string(),
        settings.retention_raw_audio_ttl_days.to_string(),
        settings.retention_transcript_ttl_days.to_string(),
        settings
            .retention_summary_ttl_days
            .map(|value| value.to_string())
            .unwrap_or_default(),
        settings.summary_enabled.to_string(),
        settings.summary_template_id.clone().unwrap_or_default(),
        settings
            .domain_knowledge_version_id
            .clone()
            .unwrap_or_default(),
    ]
}

pub struct PgSqlExecutor {
    client: Option<PgClient>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl PgSqlExecutor {
    pub fn connect(connection_str: &str) -> Result<Self, String> {
        let runtime = tokio::runtime::Runtime::new().map_err(|err| err.to_string())?;
        let conn_str = connection_str.to_owned();
        let client = std::thread::scope(|s| {
            s.spawn(|| {
                let (client, connection) = runtime
                    .block_on(tokio_postgres::connect(&conn_str, NoTls))
                    .map_err(|err| err.to_string())?;
                runtime.spawn(async move {
                    if let Err(err) = connection.await {
                        tracing::error!(error = %err, "postgres connection error");
                    }
                });
                Ok::<_, String>(client)
            })
            .join()
            .map_err(|_| "postgres connect thread panicked".to_owned())?
        })?;
        Ok(Self {
            client: Some(client),
            runtime: Some(runtime),
        })
    }

    pub fn connect_with_ssl_mode(
        base_connection_str: &str,
        ssl_mode: &str,
    ) -> Result<Self, String> {
        let conn = if base_connection_str.contains("sslmode=") {
            base_connection_str.to_owned()
        } else {
            let sep = if base_connection_str.contains('?') {
                '&'
            } else {
                '?'
            };
            format!("{base_connection_str}{sep}sslmode={ssl_mode}")
        };
        Self::connect(&conn)
    }

    fn runtime(&self) -> Result<&tokio::runtime::Runtime, String> {
        self.runtime
            .as_ref()
            .ok_or_else(|| "postgres runtime already shut down".to_owned())
    }

    fn client(&self) -> Result<&PgClient, String> {
        self.client
            .as_ref()
            .ok_or_else(|| "postgres client already shut down".to_owned())
    }
}

impl Drop for PgSqlExecutor {
    fn drop(&mut self) {
        self.client.take();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl SqlExecutor for PgSqlExecutor {
    fn execute(&mut self, sql: &str, params: &[String]) -> Result<u64, String> {
        let bind: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let client = self.client()?;
        let runtime = self.runtime()?;
        std::thread::scope(|s| {
            s.spawn(|| {
                runtime.block_on(client.execute(sql, &bind)).map_err(|err| {
                    if err.code() == Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION) {
                        format!("{UNIQUE_VIOLATION_PREFIX}{err}")
                    } else {
                        err.to_string()
                    }
                })
            })
            .join()
            .map_err(|_| "db execute thread panicked".to_owned())?
        })
    }

    fn query_active_meeting(&mut self, guild_id: &str) -> Result<Option<StoredMeeting>, String> {
        let sql = "SELECT id, guild_id, voice_channel_id, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS started_at, to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS stopped_at FROM meetings WHERE guild_id=$1 AND status IN ('scheduled','recording','stopping') ORDER BY started_at DESC LIMIT 1";
        let client = self.client()?;
        let runtime = self.runtime()?;
        std::thread::scope(|s| {
            s.spawn(|| {
                runtime
                    .block_on(client.query(sql, &[&guild_id]))
                    .map_err(|err| err.to_string())?
                    .first()
                    .map(row_to_stored_meeting)
                    .transpose()
            })
            .join()
            .map_err(|_| "db query thread panicked".to_owned())?
        })
    }

    fn query_rows(&mut self, sql: &str, params: &[String]) -> Result<Vec<SqlRow>, String> {
        let bind: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let client = self.client()?;
        let runtime = self.runtime()?;
        std::thread::scope(|s| {
            s.spawn(|| {
                runtime
                    .block_on(client.query(sql, &bind))
                    .map_err(|err| err.to_string())
                    .and_then(|rows| {
                        rows.into_iter()
                            .map(pg_row_to_optional_strings)
                            .collect::<Result<Vec<_>, _>>()
                    })
            })
            .join()
            .map_err(|_| "db query thread panicked".to_owned())?
        })
    }

    fn run_migration(&mut self, migration_sql: &str) -> Result<(), String> {
        let client = self.client()?;
        let runtime = self.runtime()?;
        std::thread::scope(|s| {
            s.spawn(|| {
                runtime
                    .block_on(client.batch_execute(migration_sql))
                    .map_err(|err| err.to_string())
            })
            .join()
            .map_err(|_| "db migration thread panicked".to_owned())?
        })
    }
}

fn parse_stop_reason_column(
    value: Option<String>,
    context: &str,
) -> Result<Option<StopReason>, String> {
    match value {
        None => Ok(None),
        Some(raw) => StopReason::parse_str(&raw)
            .map(Some)
            .ok_or_else(|| format!("invalid stop_reason for {context}: {raw}")),
    }
}

fn row_to_stored_meeting(row: &Row) -> Result<StoredMeeting, String> {
    let meeting_id: String = row.get("id");
    let status_str = row.get::<_, String>("status");
    let status = MeetingStatus::parse_str(&status_str)
        .ok_or_else(|| format!("unknown meeting status in DB: {status_str}"))?;
    let stop_reason = parse_stop_reason_column(
        row.get::<_, Option<String>>("stop_reason"),
        &format!("meeting_id={meeting_id}"),
    )?;

    Ok(StoredMeeting {
        id: row.get("id"),
        guild_id: row.get("guild_id"),
        voice_channel_id: row.get("voice_channel_id"),
        report_channel_id: row.get("report_channel_id"),
        status_message_channel_id: row.get("status_message_channel_id"),
        status_message_id: row.get("status_message_id"),
        started_by_user_id: row.get("started_by_user_id"),
        title: row.get("title"),
        status,
        stop_reason,
        error_message: row.get("error_message"),
        started_at: parse_optional_rfc3339(row.get::<_, Option<String>>("started_at")),
        stopped_at: parse_optional_rfc3339(row.get::<_, Option<String>>("stopped_at")),
    })
}

fn parse_optional_rfc3339(value: Option<String>) -> Option<DateTime<Utc>> {
    value
        .as_deref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|ts| ts.with_timezone(&Utc))
}

fn pg_row_to_optional_strings(row: Row) -> Result<SqlRow, String> {
    let mut values = Vec::with_capacity(row.len());
    for idx in 0..row.len() {
        if let Ok(v) = row.try_get::<usize, Option<String>>(idx) {
            values.push(v);
            continue;
        }
        if let Ok(v) = row.try_get::<usize, String>(idx) {
            values.push(Some(v));
            continue;
        }
        if let Ok(v) = row.try_get::<usize, i32>(idx) {
            values.push(Some(v.to_string()));
            continue;
        }
        if let Ok(v) = row.try_get::<usize, i64>(idx) {
            values.push(Some(v.to_string()));
            continue;
        }
        if let Ok(v) = row.try_get::<usize, bool>(idx) {
            values.push(Some(v.to_string()));
            continue;
        }
        if let Ok(v) = row.try_get::<usize, f64>(idx) {
            values.push(Some(v.to_string()));
            continue;
        }
        return Err(format!("unsupported postgres column type at index {idx}"));
    }
    Ok(values)
}

#[cfg(test)]
mod stop_reason_parse_tests {
    use super::parse_stop_reason_column;

    #[test]
    fn parse_stop_reason_column_rejects_unknown_values() {
        let err = parse_stop_reason_column(Some("bogus".to_owned()), "meeting_id=m1")
            .expect_err("unknown stop_reason should error");
        assert!(err.contains("invalid stop_reason"));
    }
}
