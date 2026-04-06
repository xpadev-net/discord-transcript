use crate::domain::MeetingStatus;
use crate::domain::StopReason;
use crate::domain::{JobStatus, JobType};
use crate::queue::{Job, JobQueue, QueueError};
use crate::sql::{
    CLAIM_JOB_BY_ID_SQL, CLAIM_JOB_SQL, ENQUEUE_JOB_SQL, MARK_JOB_DONE_SQL, MARK_JOB_FAILED_SQL,
    MARK_STOPPING_IF_RECORDING_SQL, RETRY_JOB_SQL,
};
use crate::storage::{
    CreateMeetingRequest, MeetingStore, StopTransition, StoreError, StoredMeeting,
};
use std::collections::HashMap;
use tokio_postgres::{Client as PgClient, NoTls, Row};

/// Prefix added to error messages when PostgreSQL returns SQLSTATE 23505
/// (unique_violation). Callers can check `err.starts_with(UNIQUE_VIOLATION_PREFIX)`
/// instead of locale-dependent string matching.
pub const UNIQUE_VIOLATION_PREFIX: &str = "UNIQUE_VIOLATION: ";

pub trait SqlExecutor {
    fn execute(&mut self, sql: &str, params: &[String]) -> Result<u64, String>;
    fn query_active_meeting(&mut self, guild_id: &str) -> Result<Option<StoredMeeting>, String>;
    fn query_rows(&mut self, sql: &str, params: &[String]) -> Result<Vec<Vec<String>>, String>;
    fn run_migration(&mut self, migration_sql: &str) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct FakeSqlExecutor {
    pub executed: Vec<(String, Vec<String>)>,
    pub active_by_guild: HashMap<String, StoredMeeting>,
    pub query_rows_result: HashMap<String, Vec<Vec<String>>>,
    pub execute_result: HashMap<String, u64>,
}

impl SqlExecutor for FakeSqlExecutor {
    fn execute(&mut self, sql: &str, params: &[String]) -> Result<u64, String> {
        self.executed.push((sql.to_owned(), params.to_vec()));
        let key = format!("{}|{}", sql, params.join("\u{1f}"));
        Ok(*self.execute_result.get(&key).unwrap_or(&1))
    }

    fn query_active_meeting(&mut self, guild_id: &str) -> Result<Option<StoredMeeting>, String> {
        Ok(self.active_by_guild.get(guild_id).cloned())
    }

    fn query_rows(&mut self, sql: &str, params: &[String]) -> Result<Vec<Vec<String>>, String> {
        self.executed.push((sql.to_owned(), params.to_vec()));
        let key = format!("{}|{}", sql, params.join("\u{1f}"));
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

impl<E: SqlExecutor> SqlMeetingStore<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }

    pub fn apply_initial_migration(&mut self, migration_sql: &str) -> Result<(), String> {
        self.executor.run_migration(migration_sql)
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
            .ok_or_else(|| QueueError::Backend("retry returned no status".to_owned()))?;
        JobStatus::parse_str(status_value).ok_or_else(|| {
            QueueError::Backend(format!(
                "unknown job status in retry result: {status_value}"
            ))
        })
    }
}

fn parse_job_row(row: &[String]) -> Result<Job, QueueError> {
    if row.len() < 6 {
        return Err(QueueError::Backend(format!(
            "invalid claimed job row length: {}",
            row.len()
        )));
    }
    let job_type = JobType::parse_str(&row[2])
        .ok_or_else(|| QueueError::Backend(format!("unknown job type: {}", row[2])))?;
    let status = JobStatus::parse_str(&row[3])
        .ok_or_else(|| QueueError::Backend(format!("unknown job status: {}", row[3])))?;
    let retry_count = row[4]
        .parse::<u32>()
        .map_err(|err| QueueError::Backend(format!("invalid retry_count '{}': {err}", row[4])))?;
    let error_message = if row[5].trim().is_empty() {
        None
    } else {
        Some(row[5].clone())
    };

    Ok(Job {
        id: row[0].clone(),
        meeting_id: row[1].clone(),
        job_type,
        status,
        retry_count,
        error_message,
    })
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
                "SELECT id, guild_id, voice_channel_id, report_channel_id, started_by_user_id, title, status, stop_reason, error_message \
                 FROM meetings WHERE id=$1 LIMIT 1",
                &[meeting_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        if row.len() < 9 {
            return Err(StoreError::Backend(format!(
                "invalid meeting row length for id={meeting_id}: {}",
                row.len()
            )));
        }
        let status = MeetingStatus::parse_str(&row[6]).ok_or_else(|| {
            StoreError::Backend(format!(
                "invalid meeting status for id={meeting_id}: {}",
                row[6]
            ))
        })?;
        let stop_reason = if row[7].trim().is_empty() {
            None
        } else {
            Some(StopReason::parse_str(&row[7]).ok_or_else(|| {
                StoreError::Backend(format!(
                    "invalid stop_reason for id={meeting_id}: {}",
                    row[7]
                ))
            })?)
        };
        let error_message = if row[8].trim().is_empty() {
            None
        } else {
            Some(row[8].clone())
        };
        Ok(Some(StoredMeeting {
            id: row[0].clone(),
            guild_id: row[1].clone(),
            voice_channel_id: row[2].clone(),
            report_channel_id: row[3].clone(),
            started_by_user_id: row[4].clone(),
            title: if row[5].trim().is_empty() {
                None
            } else {
                Some(row[5].clone())
            },
            status,
            stop_reason,
            error_message,
        }))
    }

    fn create_scheduled_meeting(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError> {
        let sql = "INSERT INTO meetings(id,guild_id,voice_channel_id,report_channel_id,started_by_user_id,status) VALUES($1,$2,$3,$4,$5,'scheduled')";
        let meeting_id = request.id.clone();
        self.executor
            .execute(
                sql,
                &[
                    request.id,
                    request.guild_id,
                    request.voice_channel_id,
                    request.report_channel_id,
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
        Ok(())
    }

    fn create_meeting_as_recording(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError> {
        let sql = "INSERT INTO meetings(id,guild_id,voice_channel_id,report_channel_id,started_by_user_id,status) VALUES($1,$2,$3,$4,$5,'recording')";
        let meeting_id = request.id.clone();
        self.executor
            .execute(
                sql,
                &[
                    request.id,
                    request.guild_id,
                    request.voice_channel_id,
                    request.report_channel_id,
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
                        "WITH updated AS (UPDATE meetings SET status=$1, updated_at=NOW() WHERE id=$2 AND status=$3 RETURNING 1), existing AS (SELECT 1 FROM meetings WHERE id=$2) SELECT CASE WHEN EXISTS (SELECT 1 FROM updated) THEN 'updated' WHEN EXISTS (SELECT 1 FROM existing) THEN 'conflict' ELSE 'not_found' END",
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
                    .map(|value| value.as_str())
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
        let sql = "SELECT id, guild_id, voice_channel_id, report_channel_id, started_by_user_id, title, status, stop_reason, error_message FROM meetings WHERE guild_id=$1 AND status IN ('scheduled','recording','stopping') ORDER BY started_at DESC LIMIT 1";
        let client = self.client()?;
        let runtime = self.runtime()?;
        std::thread::scope(|s| {
            s.spawn(|| {
                runtime
                    .block_on(client.query(sql, &[&guild_id]))
                    .map(|rows| rows.first().map(row_to_stored_meeting))
                    .map_err(|err| err.to_string())
            })
            .join()
            .map_err(|_| "db query thread panicked".to_owned())?
        })
    }

    fn query_rows(&mut self, sql: &str, params: &[String]) -> Result<Vec<Vec<String>>, String> {
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
                            .map(pg_row_to_strings)
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

fn row_to_stored_meeting(row: &Row) -> StoredMeeting {
    let status_str = row.get::<_, String>("status");
    let status = MeetingStatus::parse_str(&status_str).unwrap_or_else(|| {
        tracing::warn!(status = %status_str, "unknown meeting status in DB, defaulting to Aborted");
        MeetingStatus::Aborted
    });
    let stop_reason = row
        .get::<_, Option<String>>("stop_reason")
        .as_deref()
        .and_then(StopReason::parse_str);

    StoredMeeting {
        id: row.get("id"),
        guild_id: row.get("guild_id"),
        voice_channel_id: row.get("voice_channel_id"),
        report_channel_id: row.get("report_channel_id"),
        started_by_user_id: row.get("started_by_user_id"),
        title: row.get("title"),
        status,
        stop_reason,
        error_message: row.get("error_message"),
    }
}

fn pg_row_to_strings(row: Row) -> Result<Vec<String>, String> {
    let mut values = Vec::with_capacity(row.len());
    for idx in 0..row.len() {
        if let Ok(v) = row.try_get::<usize, Option<String>>(idx) {
            values.push(v.unwrap_or_default());
            continue;
        }
        if let Ok(v) = row.try_get::<usize, String>(idx) {
            values.push(v);
            continue;
        }
        if let Ok(v) = row.try_get::<usize, i32>(idx) {
            values.push(v.to_string());
            continue;
        }
        if let Ok(v) = row.try_get::<usize, i64>(idx) {
            values.push(v.to_string());
            continue;
        }
        if let Ok(v) = row.try_get::<usize, bool>(idx) {
            values.push(v.to_string());
            continue;
        }
        if let Ok(v) = row.try_get::<usize, f64>(idx) {
            values.push(v.to_string());
            continue;
        }
        return Err(format!("unsupported postgres column type at index {idx}"));
    }
    Ok(values)
}
