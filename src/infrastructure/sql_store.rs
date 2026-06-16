use crate::domain::MeetingStatus;
use crate::domain::StopReason;
use crate::domain::ai_memory::{
    AiMemoryNote, AiMemorySourceType, AiMemoryTag, NewAiMemoryNote, UpdateAiMemoryNote,
};
use crate::domain::audit::AuditEvent;
use crate::domain::authz::{RbacPermission, RbacRoleGrant};
use crate::domain::confidence::ConfidencePermille;
use crate::domain::domain_knowledge::{
    DomainKnowledgeContentType, DomainKnowledgeItem, NewDomainKnowledgeItem,
    UpdateDomainKnowledgeItem,
};
use crate::domain::feedback::{
    NewTranscriptFeedback, TranscriptFeedback, TranscriptFeedbackStatus,
    TranscriptFeedbackTermType, TranscriptFeedbackType, UpdateTranscriptFeedbackStatus,
};
use crate::domain::person_alias::{
    NewPersonAlias, PersonAlias, PersonAliasReviewStatus, PersonAliasSourceType, UpdatePersonAlias,
};
use crate::domain::plans::{
    PlanFallback, PlanKind, PlanQuota, QuotaDimension, QuotaEnforcementMode, QuotaPeriod,
    ResolvedPlan,
};
use crate::domain::summary_template::{NewSummaryTemplate, SummaryTemplate, UpdateSummaryTemplate};
use crate::domain::usage::{NewUsageEvent, UsageAggregate, UsageEvent, UsageMetric};
use crate::domain::{JobStatus, JobType};
use crate::infrastructure::queue::{Job, JobQueue, QueueError, require_claim_token};
use crate::infrastructure::sql::{
    ACTIVATE_DOMAIN_KNOWLEDGE_SQL, ACTIVATE_SUMMARY_TEMPLATE_SQL, ACTIVE_MEETING_UNIQUE_INDEX_NAME,
    AGGREGATE_RECENT_USAGE_SQL, ARCHIVE_AI_MEMORY_NOTE_SQL, ARCHIVE_DOMAIN_KNOWLEDGE_SQL,
    ARCHIVE_PERSON_ALIAS_SQL, ARCHIVE_SUMMARY_TEMPLATE_SQL,
    BACKFILL_DEFAULT_TENANTS_FROM_EXISTING_GUILDS_SQL, CLAIM_JOB_BY_ID_SQL, CLAIM_JOB_SQL,
    CREATE_SCHEMA_MIGRATIONS_SQL, ENQUEUE_JOB_SQL, FIND_ACTIVE_RECORDING_BLOCKER_BY_GUILD_SQL,
    GET_ACTIVE_SUMMARY_TEMPLATE_SQL, GET_AI_MEMORY_NOTE_SQL, GET_DOMAIN_KNOWLEDGE_SQL,
    GET_EFFECTIVE_MEETING_SETTINGS_SQL, GET_GUILD_SETTINGS_FOR_MEETING_SNAPSHOT_SQL,
    GET_SUMMARY_TEMPLATE_SQL, HEARTBEAT_RUNNING_JOB_SQL, INSERT_AI_MEMORY_NOTE_SQL,
    INSERT_AUDIT_EVENT_SQL, INSERT_DOMAIN_KNOWLEDGE_SQL, INSERT_PERSON_ALIAS_SQL,
    INSERT_RECORDING_MEETING_WITH_EFFECTIVE_SETTINGS_SQL,
    INSERT_SCHEDULED_MEETING_WITH_EFFECTIVE_SETTINGS_SQL, INSERT_SUMMARY_TEMPLATE_SQL,
    INSERT_TRANSCRIPT_FEEDBACK_SQL, INSERT_USAGE_EVENT_SQL, LIST_AI_MEMORY_NOTES_SQL,
    LIST_DOMAIN_KNOWLEDGE_SQL, LIST_GUILD_RBAC_PERMISSIONS_FOR_ROLE_CSV_SQL,
    LIST_PERSON_ALIASES_SQL, LIST_RECENT_AUDIT_EVENTS_SQL, LIST_RECENT_USAGE_EVENTS_SQL,
    LIST_SUMMARY_TEMPLATES_SQL, LIST_TRANSCRIPT_FEEDBACK_SQL, LOCK_SCHEMA_MIGRATIONS_SQL,
    MARK_JOB_DONE_SQL, MARK_JOB_FAILED_SQL, MARK_STOPPING_IF_RECORDING_SQL, MIGRATIONS, Migration,
    RECOVERY_READY_SUMMARY_JOBS_SQL, RESOLVE_PLAN_FOR_GUILD_SQL, RESOLVE_TENANT_BY_GUILD_SQL,
    RETRY_JOB_SQL, SELECT_SCHEMA_MIGRATION_SQL, SET_AI_MEMORY_PINNED_SQL,
    SET_MEETING_STATUS_CAS_SQL, UNLOCK_SCHEMA_MIGRATIONS_SQL, UPDATE_AI_MEMORY_NOTE_SQL,
    UPDATE_DOMAIN_KNOWLEDGE_SQL, UPDATE_PERSON_ALIAS_SQL, UPDATE_SUMMARY_TEMPLATE_SQL,
    UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL, UPSERT_EFFECTIVE_MEETING_SETTINGS_SQL,
    UPSERT_VC_PARTICIPANT_PERSON_ALIAS_CANDIDATE_SQL, migration_transaction_sql,
};
use crate::infrastructure::storage::{
    CreateMeetingRequest, EffectiveMeetingSettings, GuildSettingsForSnapshot, MeetingStore,
    StatusMessageMetadata, StopTransition, StoreError, StoredMeeting, UsageEventStore,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::str::FromStr;
use tokio_postgres::{Client as PgClient, NoTls, Row};

const MAX_POSTGRES_INTERVAL_SECONDS: u64 = i64::MAX as u64 / 1_000_000;

/// Prefix added to error messages when PostgreSQL returns SQLSTATE 23505
/// (unique_violation). Callers can check `err.starts_with(UNIQUE_VIOLATION_PREFIX)`
/// instead of locale-dependent string matching.
pub const UNIQUE_VIOLATION_PREFIX: &str = "UNIQUE_VIOLATION: ";
const UNIQUE_VIOLATION_CONSTRAINT_SEPARATOR: &str = ": ";

pub fn unique_violation_constraint(error: &str) -> Option<&str> {
    error
        .strip_prefix(UNIQUE_VIOLATION_PREFIX)?
        .split_once(UNIQUE_VIOLATION_CONSTRAINT_SEPARATOR)
        .map(|(constraint, _)| constraint)
        .filter(|constraint| !constraint.is_empty())
}

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
        // Wildcard keys support SQL whose params include generated values such as
        // claim tokens. They intentionally bypass exact parameter arity checks.
        let wildcard_key = format!("{sql}|*");
        Ok(*self
            .execute_result
            .get(&key)
            .or_else(|| self.execute_result.get(&wildcard_key))
            .unwrap_or(&1))
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
        // Wildcard keys support SQL whose params include generated values such as
        // claim tokens. They intentionally bypass exact parameter arity checks.
        let wildcard_key = format!("{sql}|*");
        Ok(self
            .query_rows_result
            .get(&key)
            .or_else(|| self.query_rows_result.get(&wildcard_key))
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

    pub fn apply_pending_migrations(&mut self) -> Result<(), String> {
        self.executor.run_migration(LOCK_SCHEMA_MIGRATIONS_SQL)?;
        let result = self.apply_pending_migrations_locked();
        let unlock_result = self.executor.run_migration(UNLOCK_SCHEMA_MIGRATIONS_SQL);
        match (result, unlock_result) {
            (Err(err), _) => Err(err),
            (Ok(()), Err(err)) => Err(err),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn apply_pending_migrations_locked(&mut self) -> Result<(), String> {
        self.executor.run_migration(CREATE_SCHEMA_MIGRATIONS_SQL)?;
        for migration in MIGRATIONS {
            self.apply_pending_migration(*migration)?;
        }
        Ok(())
    }

    fn apply_pending_migration(&mut self, migration: Migration) -> Result<(), String> {
        if !self
            .executor
            .query_rows(SELECT_SCHEMA_MIGRATION_SQL, &[migration.version.to_owned()])?
            .is_empty()
        {
            return Ok(());
        }
        self.executor
            .run_migration(&migration_transaction_sql(migration))
    }

    fn map_active_meeting_insert_error(
        &mut self,
        err: String,
        meeting_id: String,
        guild_id: String,
    ) -> StoreError {
        if unique_violation_constraint(&err) == Some(ACTIVE_MEETING_UNIQUE_INDEX_NAME) {
            return match self.find_active_recording_blocker_by_guild(&guild_id) {
                Ok(Some(active)) => StoreError::ActiveMeetingExists {
                    meeting_id: active.id,
                },
                Ok(None) => StoreError::Backend(err),
                Err(query_err) => query_err,
            };
        }

        if err.starts_with(UNIQUE_VIOLATION_PREFIX) {
            StoreError::AlreadyExists { meeting_id }
        } else {
            StoreError::Backend(err)
        }
    }

    fn find_active_recording_blocker_by_guild(
        &mut self,
        guild_id: &str,
    ) -> Result<Option<StoredMeeting>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                FIND_ACTIVE_RECORDING_BLOCKER_BY_GUILD_SQL,
                &[guild_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        sql_row_to_stored_meeting(&row, &format!("guild_id={guild_id}")).map(Some)
    }

    pub fn list_rbac_role_grants_for_member_roles(
        &mut self,
        guild_id: &str,
        member_roles: &[String],
    ) -> Result<Vec<RbacRoleGrant>, StoreError> {
        if member_roles.is_empty() {
            return Ok(Vec::new());
        }
        let role_csv = member_roles.join(",");
        let rows = self
            .executor
            .query_rows(
                LIST_GUILD_RBAC_PERMISSIONS_FOR_ROLE_CSV_SQL,
                &[guild_id.to_owned(), role_csv],
            )
            .map_err(StoreError::Backend)?;
        rows.into_iter()
            .map(|row| {
                if row.len() < 3 {
                    return Err(StoreError::Backend(format!(
                        "invalid RBAC grant row length for guild={guild_id}: {}",
                        row.len()
                    )));
                }
                let guild_id = require_store_column(&row, 0, "guild_id")?;
                let discord_role_id = require_store_column(&row, 1, "discord_role_id")?;
                let permission_name = require_store_column(&row, 2, "permission_name")?;
                let permission = RbacPermission::from_str(&permission_name).map_err(|err| {
                    StoreError::Backend(format!(
                        "invalid RBAC permission '{permission_name}' for guild={guild_id}: {err}"
                    ))
                })?;
                Ok(RbacRoleGrant {
                    guild_id,
                    discord_role_id,
                    permission,
                })
            })
            .collect()
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

    pub fn resolve_plan_for_guild(
        &mut self,
        guild_id: &str,
        fallback: PlanFallback,
        at: DateTime<Utc>,
    ) -> Result<Option<ResolvedPlan>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                RESOLVE_PLAN_FOR_GUILD_SQL,
                &[
                    guild_id.to_owned(),
                    at.to_rfc3339(),
                    fallback.as_str().to_owned(),
                ],
            )
            .map_err(StoreError::Backend)?;
        parse_resolved_plan_rows(rows)
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

    pub fn append_audit_event(&mut self, event: &AuditEvent) -> Result<(), StoreError> {
        self.executor
            .execute(INSERT_AUDIT_EVENT_SQL, &audit_event_params(event))
            .map_err(StoreError::Backend)?;
        Ok(())
    }

    pub fn list_recent_audit_events(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AuditEvent>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                LIST_RECENT_AUDIT_EVENTS_SQL,
                &[
                    tenant_id.unwrap_or_default().to_owned(),
                    guild_id.unwrap_or_default().to_owned(),
                    limit.min(100).to_string(),
                ],
            )
            .map_err(StoreError::Backend)?;
        rows.iter().map(parse_audit_event_row).collect()
    }

    pub fn append_usage_event(&mut self, event: &NewUsageEvent) -> Result<(), StoreError> {
        self.executor
            .execute(INSERT_USAGE_EVENT_SQL, &usage_event_params(event))
            .map_err(StoreError::Backend)?;
        Ok(())
    }

    pub fn list_recent_usage_events(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<UsageEvent>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                LIST_RECENT_USAGE_EVENTS_SQL,
                &[
                    tenant_id.unwrap_or_default().to_owned(),
                    guild_id.unwrap_or_default().to_owned(),
                    limit.min(100).to_string(),
                ],
            )
            .map_err(StoreError::Backend)?;
        rows.iter().map(parse_usage_event_row).collect()
    }

    pub fn aggregate_recent_usage(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        window_seconds: u64,
    ) -> Result<Vec<UsageAggregate>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                AGGREGATE_RECENT_USAGE_SQL,
                &[
                    tenant_id.unwrap_or_default().to_owned(),
                    guild_id.unwrap_or_default().to_owned(),
                    window_seconds
                        .clamp(1, MAX_POSTGRES_INTERVAL_SECONDS)
                        .to_string(),
                ],
            )
            .map_err(StoreError::Backend)?;
        rows.iter().map(parse_usage_aggregate_row).collect()
    }

    pub fn list_domain_knowledge(
        &mut self,
        guild_id: &str,
        include_archived: bool,
        content_type: Option<DomainKnowledgeContentType>,
    ) -> Result<Vec<DomainKnowledgeItem>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                LIST_DOMAIN_KNOWLEDGE_SQL,
                &[
                    guild_id.to_owned(),
                    include_archived.to_string(),
                    content_type
                        .map(|content_type| content_type.as_str().to_owned())
                        .unwrap_or_default(),
                ],
            )
            .map_err(StoreError::Backend)?;
        rows.iter().map(parse_domain_knowledge_row).collect()
    }

    pub fn get_domain_knowledge(
        &mut self,
        guild_id: &str,
        id: &str,
    ) -> Result<Option<DomainKnowledgeItem>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                GET_DOMAIN_KNOWLEDGE_SQL,
                &[guild_id.to_owned(), id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_domain_knowledge_row(&row).map(Some)
    }

    pub fn create_domain_knowledge(
        &mut self,
        item: &NewDomainKnowledgeItem,
    ) -> Result<DomainKnowledgeItem, StoreError> {
        let rows = self
            .executor
            .query_rows(
                INSERT_DOMAIN_KNOWLEDGE_SQL,
                &new_domain_knowledge_params(item),
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(StoreError::Backend(
                "domain knowledge insert returned no row".to_owned(),
            ));
        };
        parse_domain_knowledge_row(&row)
    }

    pub fn update_domain_knowledge(
        &mut self,
        item: &UpdateDomainKnowledgeItem,
    ) -> Result<Option<DomainKnowledgeItem>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                UPDATE_DOMAIN_KNOWLEDGE_SQL,
                &update_domain_knowledge_params(item),
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_domain_knowledge_row(&row).map(Some)
    }

    pub fn activate_domain_knowledge(
        &mut self,
        guild_id: &str,
        id: &str,
        actor_user_id: Option<&str>,
    ) -> Result<Option<DomainKnowledgeItem>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                ACTIVATE_DOMAIN_KNOWLEDGE_SQL,
                &[
                    id.to_owned(),
                    guild_id.to_owned(),
                    actor_user_id.unwrap_or_default().to_owned(),
                ],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_domain_knowledge_row(&row).map(Some)
    }

    pub fn archive_domain_knowledge(
        &mut self,
        guild_id: &str,
        id: &str,
        actor_user_id: &str,
    ) -> Result<Option<DomainKnowledgeItem>, StoreError> {
        if actor_user_id.trim().is_empty() {
            return Err(StoreError::Backend(
                "domain knowledge archive actor is required".to_owned(),
            ));
        }
        let rows = self
            .executor
            .query_rows(
                ARCHIVE_DOMAIN_KNOWLEDGE_SQL,
                &[id.to_owned(), guild_id.to_owned(), actor_user_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_domain_knowledge_row(&row).map(Some)
    }

    pub fn list_ai_memory_notes(
        &mut self,
        tenant_id: &str,
        guild_id: &str,
        include_archived: bool,
        source_type: Option<AiMemorySourceType>,
    ) -> Result<Vec<AiMemoryNote>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                LIST_AI_MEMORY_NOTES_SQL,
                &[
                    tenant_id.to_owned(),
                    guild_id.to_owned(),
                    include_archived.to_string(),
                    source_type
                        .map(|source_type| source_type.as_str().to_owned())
                        .unwrap_or_default(),
                ],
            )
            .map_err(StoreError::Backend)?;
        rows.iter().map(parse_ai_memory_note_row).collect()
    }

    pub fn get_ai_memory_note(
        &mut self,
        tenant_id: &str,
        guild_id: &str,
        id: &str,
    ) -> Result<Option<AiMemoryNote>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                GET_AI_MEMORY_NOTE_SQL,
                &[id.to_owned(), tenant_id.to_owned(), guild_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_ai_memory_note_row(&row).map(Some)
    }

    pub fn create_ai_memory_note(
        &mut self,
        item: &NewAiMemoryNote,
    ) -> Result<AiMemoryNote, StoreError> {
        let rows = self
            .executor
            .query_rows(INSERT_AI_MEMORY_NOTE_SQL, &new_ai_memory_note_params(item))
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(StoreError::Backend(
                "ai memory insert returned no row".to_owned(),
            ));
        };
        parse_ai_memory_note_row(&row)
    }

    pub fn update_ai_memory_note(
        &mut self,
        item: &UpdateAiMemoryNote,
    ) -> Result<Option<AiMemoryNote>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                UPDATE_AI_MEMORY_NOTE_SQL,
                &update_ai_memory_note_params(item),
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_ai_memory_note_row(&row).map(Some)
    }

    pub fn set_ai_memory_pinned(
        &mut self,
        tenant_id: &str,
        guild_id: &str,
        id: &str,
        pinned: bool,
        actor_user_id: &str,
    ) -> Result<Option<AiMemoryNote>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                SET_AI_MEMORY_PINNED_SQL,
                &[
                    id.to_owned(),
                    tenant_id.to_owned(),
                    guild_id.to_owned(),
                    pinned.to_string(),
                    actor_user_id.to_owned(),
                ],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_ai_memory_note_row(&row).map(Some)
    }

    pub fn archive_ai_memory_note(
        &mut self,
        tenant_id: &str,
        guild_id: &str,
        id: &str,
        actor_user_id: &str,
    ) -> Result<Option<AiMemoryNote>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                ARCHIVE_AI_MEMORY_NOTE_SQL,
                &[
                    id.to_owned(),
                    tenant_id.to_owned(),
                    guild_id.to_owned(),
                    actor_user_id.to_owned(),
                ],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_ai_memory_note_row(&row).map(Some)
    }

    pub fn create_transcript_feedback(
        &mut self,
        item: &NewTranscriptFeedback,
    ) -> Result<TranscriptFeedback, StoreError> {
        let rows = self
            .executor
            .query_rows(
                INSERT_TRANSCRIPT_FEEDBACK_SQL,
                &new_transcript_feedback_params(item),
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(StoreError::Backend(
                "transcript feedback insert returned no row".to_owned(),
            ));
        };
        parse_transcript_feedback_row(&row)
    }

    pub fn list_transcript_feedback(
        &mut self,
        tenant_id: &str,
        guild_id: &str,
        status: Option<TranscriptFeedbackStatus>,
        feedback_type: Option<TranscriptFeedbackType>,
    ) -> Result<Vec<TranscriptFeedback>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                LIST_TRANSCRIPT_FEEDBACK_SQL,
                &[
                    tenant_id.to_owned(),
                    guild_id.to_owned(),
                    status
                        .map(|status| status.as_str().to_owned())
                        .unwrap_or_default(),
                    feedback_type
                        .map(|feedback_type| feedback_type.as_str().to_owned())
                        .unwrap_or_default(),
                ],
            )
            .map_err(StoreError::Backend)?;
        rows.iter().map(parse_transcript_feedback_row).collect()
    }

    pub fn update_transcript_feedback_status(
        &mut self,
        item: &UpdateTranscriptFeedbackStatus,
    ) -> Result<Option<TranscriptFeedback>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                UPDATE_TRANSCRIPT_FEEDBACK_STATUS_SQL,
                &update_transcript_feedback_status_params(item),
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_transcript_feedback_row(&row).map(Some)
    }

    pub fn list_person_aliases(
        &mut self,
        tenant_id: &str,
        guild_id: &str,
        include_archived: bool,
        review_status: Option<PersonAliasReviewStatus>,
    ) -> Result<Vec<PersonAlias>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                LIST_PERSON_ALIASES_SQL,
                &[
                    tenant_id.to_owned(),
                    guild_id.to_owned(),
                    include_archived.to_string(),
                    review_status
                        .map(|status| status.as_str().to_owned())
                        .unwrap_or_default(),
                ],
            )
            .map_err(StoreError::Backend)?;
        rows.iter().map(parse_person_alias_row).collect()
    }

    pub fn create_person_alias(
        &mut self,
        item: &NewPersonAlias,
    ) -> Result<PersonAlias, StoreError> {
        let rows = self
            .executor
            .query_rows(INSERT_PERSON_ALIAS_SQL, &new_person_alias_params(item))
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(StoreError::Backend(
                "person alias insert returned no row".to_owned(),
            ));
        };
        parse_person_alias_row(&row)
    }

    pub fn upsert_vc_participant_person_alias_candidate(
        &mut self,
        item: &NewPersonAlias,
    ) -> Result<Option<PersonAlias>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                UPSERT_VC_PARTICIPANT_PERSON_ALIAS_CANDIDATE_SQL,
                &new_person_alias_params(item),
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_person_alias_row(&row).map(Some)
    }

    pub fn update_person_alias(
        &mut self,
        item: &UpdatePersonAlias,
    ) -> Result<Option<PersonAlias>, StoreError> {
        let rows = self
            .executor
            .query_rows(UPDATE_PERSON_ALIAS_SQL, &update_person_alias_params(item))
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_person_alias_row(&row).map(Some)
    }

    pub fn archive_person_alias(
        &mut self,
        tenant_id: &str,
        guild_id: &str,
        id: &str,
        actor_user_id: &str,
    ) -> Result<Option<PersonAlias>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                ARCHIVE_PERSON_ALIAS_SQL,
                &[
                    id.to_owned(),
                    tenant_id.to_owned(),
                    guild_id.to_owned(),
                    actor_user_id.to_owned(),
                ],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_person_alias_row(&row).map(Some)
    }

    pub fn list_summary_templates(
        &mut self,
        guild_id: &str,
        include_archived: bool,
    ) -> Result<Vec<SummaryTemplate>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                LIST_SUMMARY_TEMPLATES_SQL,
                &[guild_id.to_owned(), include_archived.to_string()],
            )
            .map_err(StoreError::Backend)?;
        rows.iter().map(parse_summary_template_row).collect()
    }

    pub fn get_summary_template(
        &mut self,
        guild_id: &str,
        id: &str,
    ) -> Result<Option<SummaryTemplate>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                GET_SUMMARY_TEMPLATE_SQL,
                &[guild_id.to_owned(), id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_summary_template_row(&row).map(Some)
    }

    pub fn get_active_summary_template(
        &mut self,
        guild_id: &str,
    ) -> Result<Option<SummaryTemplate>, StoreError> {
        let rows = self
            .executor
            .query_rows(GET_ACTIVE_SUMMARY_TEMPLATE_SQL, &[guild_id.to_owned()])
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_summary_template_row(&row).map(Some)
    }

    pub fn create_summary_template(
        &mut self,
        item: &NewSummaryTemplate,
    ) -> Result<SummaryTemplate, StoreError> {
        let rows = self
            .executor
            .query_rows(
                INSERT_SUMMARY_TEMPLATE_SQL,
                &new_summary_template_params(item),
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(StoreError::Backend(
                "summary template insert returned no row".to_owned(),
            ));
        };
        parse_summary_template_row(&row)
    }

    pub fn update_summary_template(
        &mut self,
        item: &UpdateSummaryTemplate,
    ) -> Result<Option<SummaryTemplate>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                UPDATE_SUMMARY_TEMPLATE_SQL,
                &update_summary_template_params(item),
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_summary_template_row(&row).map(Some)
    }

    pub fn activate_summary_template(
        &mut self,
        guild_id: &str,
        id: &str,
        actor_user_id: Option<&str>,
    ) -> Result<Option<SummaryTemplate>, StoreError> {
        let rows = self
            .executor
            .query_rows(
                ACTIVATE_SUMMARY_TEMPLATE_SQL,
                &[
                    id.to_owned(),
                    guild_id.to_owned(),
                    actor_user_id.unwrap_or_default().to_owned(),
                ],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_summary_template_row(&row).map(Some)
    }

    pub fn archive_summary_template(
        &mut self,
        guild_id: &str,
        id: &str,
        actor_user_id: &str,
    ) -> Result<Option<SummaryTemplate>, StoreError> {
        if actor_user_id.trim().is_empty() {
            return Err(StoreError::Backend(
                "summary template archive actor is required".to_owned(),
            ));
        }
        let rows = self
            .executor
            .query_rows(
                ARCHIVE_SUMMARY_TEMPLATE_SQL,
                &[id.to_owned(), guild_id.to_owned(), actor_user_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_summary_template_row(&row).map(Some)
    }
}

impl<E: SqlExecutor> UsageEventStore for SqlMeetingStore<E> {
    fn append_usage_event(&mut self, event: &NewUsageEvent) -> Result<(), StoreError> {
        SqlMeetingStore::append_usage_event(self, event)
    }

    fn list_recent_usage_events(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<UsageEvent>, StoreError> {
        SqlMeetingStore::list_recent_usage_events(self, tenant_id, guild_id, limit)
    }

    fn aggregate_recent_usage(
        &mut self,
        tenant_id: Option<&str>,
        guild_id: Option<&str>,
        window_seconds: u64,
    ) -> Result<Vec<UsageAggregate>, StoreError> {
        SqlMeetingStore::aggregate_recent_usage(self, tenant_id, guild_id, window_seconds)
    }
}

pub struct SqlJobQueue<E: SqlExecutor> {
    pub executor: E,
}

impl<E: SqlExecutor> SqlJobQueue<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }

    pub fn ready_summary_meeting_ids(&mut self) -> Result<Vec<String>, QueueError> {
        self.executor
            .query_rows(RECOVERY_READY_SUMMARY_JOBS_SQL, &[])
            .map(|rows| {
                rows.into_iter()
                    .filter_map(|row| row.first().and_then(|value| value.clone()))
                    .collect()
            })
            .map_err(QueueError::Backend)
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
        let claim_token = uuid::Uuid::new_v4().to_string();
        let rows = self
            .executor
            .query_rows(CLAIM_JOB_SQL, &[job_type.as_str().to_owned(), claim_token])
            .map_err(QueueError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_job_row(&row).map(Some)
    }

    fn claim_by_id(&mut self, job_id: &str) -> Result<Option<Job>, QueueError> {
        let claim_token = uuid::Uuid::new_v4().to_string();
        let rows = self
            .executor
            .query_rows(CLAIM_JOB_BY_ID_SQL, &[job_id.to_owned(), claim_token])
            .map_err(QueueError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        parse_job_row(&row).map(Some)
    }

    fn heartbeat(&mut self, job: &Job) -> Result<(), QueueError> {
        let claim_token = require_claim_token(job)?;
        let affected = self
            .executor
            .execute(
                HEARTBEAT_RUNNING_JOB_SQL,
                &[job.id.clone(), claim_token.to_owned()],
            )
            .map_err(QueueError::Backend)?;
        if affected == 0 {
            // SQL cannot distinguish missing, non-running, and claim-token mismatch
            // without an extra read; for heartbeat callers the actionable state is
            // that this claim no longer owns a running job.
            return Err(QueueError::InvalidState {
                job_id: job.id.clone(),
                expected: "running with matching claim token".to_owned(),
                actual: "missing, not running, or different claim token".to_owned(),
            });
        }
        Ok(())
    }

    fn mark_done(&mut self, job: &Job) -> Result<(), QueueError> {
        let claim_token = require_claim_token(job)?;
        let affected = self
            .executor
            .execute(MARK_JOB_DONE_SQL, &[job.id.clone(), claim_token.to_owned()])
            .map_err(QueueError::Backend)?;
        if affected == 0 {
            return Err(QueueError::InvalidState {
                job_id: job.id.clone(),
                expected: "running with matching claim token".to_owned(),
                actual: "missing, not running, or different claim token".to_owned(),
            });
        }
        Ok(())
    }

    fn mark_failed(&mut self, job: &Job, error_message: String) -> Result<(), QueueError> {
        let claim_token = require_claim_token(job)?;
        let affected = self
            .executor
            .execute(
                MARK_JOB_FAILED_SQL,
                &[job.id.clone(), error_message, claim_token.to_owned()],
            )
            .map_err(QueueError::Backend)?;
        if affected == 0 {
            return Err(QueueError::InvalidState {
                job_id: job.id.clone(),
                expected: "running with matching claim token".to_owned(),
                actual: "missing, not running, or different claim token".to_owned(),
            });
        }
        Ok(())
    }

    fn retry(
        &mut self,
        job: &Job,
        error_message: String,
        max_retries: u32,
    ) -> Result<JobStatus, QueueError> {
        let claim_token = require_claim_token(job)?;
        let rows = self
            .executor
            .query_rows(
                RETRY_JOB_SQL,
                &[
                    job.id.clone(),
                    error_message,
                    max_retries.to_string(),
                    claim_token.to_owned(),
                ],
            )
            .map_err(QueueError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(QueueError::InvalidState {
                job_id: job.id.clone(),
                expected: "running with matching claim token".to_owned(),
                actual: "missing, not running, or different claim token".to_owned(),
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
    if row.len() < 7 {
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
        claim_token: row.get(6).and_then(|v| v.clone()),
        next_run_at: parse_optional_job_timestamp(row.get(7).and_then(|v| v.clone()))?,
    })
}

fn parse_optional_job_timestamp(
    value: Option<String>,
) -> Result<Option<DateTime<Utc>>, QueueError> {
    let Some(value) = value else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(&value)
        .map(|timestamp| Some(timestamp.with_timezone(&Utc)))
        .map_err(|err| QueueError::Backend(format!("invalid job timestamp '{value}': {err}")))
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPlanHeader {
    assignment_id: Option<String>,
    tenant_id: Option<String>,
    guild_id: String,
    plan_id: String,
    plan_code: String,
    plan_name: String,
    plan_kind: PlanKind,
    resolution_source: String,
    assignment_source: Option<String>,
    period_anchor: Option<DateTime<Utc>>,
    valid_from: Option<DateTime<Utc>>,
    valid_until: Option<DateTime<Utc>>,
}

impl ResolvedPlanHeader {
    fn into_resolved_plan(self) -> ResolvedPlan {
        ResolvedPlan {
            assignment_id: self.assignment_id,
            tenant_id: self.tenant_id,
            guild_id: self.guild_id,
            plan_id: self.plan_id,
            plan_code: self.plan_code,
            plan_name: self.plan_name,
            plan_kind: self.plan_kind,
            resolution_source: self.resolution_source,
            assignment_source: self.assignment_source,
            period_anchor: self.period_anchor,
            valid_from: self.valid_from,
            valid_until: self.valid_until,
            quotas: Vec::new(),
        }
    }
}

fn parse_resolved_plan_rows(rows: Vec<SqlRow>) -> Result<Option<ResolvedPlan>, StoreError> {
    let mut rows = rows.iter();
    let Some(first) = rows.next() else {
        return Ok(None);
    };
    let first_header = parse_resolved_plan_header(first)?;
    let mut resolved = first_header.clone().into_resolved_plan();
    if let Some(quota) = parse_resolved_plan_quota_row(first)? {
        resolved.quotas.push(quota);
    }
    for row in rows {
        let row_header = parse_resolved_plan_header(row)?;
        if row_header != first_header {
            return Err(StoreError::Backend(
                "plan resolver returned rows for multiple plans or assignments".to_owned(),
            ));
        }
        if let Some(quota) = parse_resolved_plan_quota_row(row)? {
            resolved.quotas.push(quota);
        }
    }
    Ok(Some(resolved))
}

fn parse_resolved_plan_header(row: &SqlRow) -> Result<ResolvedPlanHeader, StoreError> {
    if row.len() < 18 {
        return Err(StoreError::Backend(format!(
            "invalid plan resolver row length: {}",
            row.len()
        )));
    }
    let plan_kind_raw = require_store_column(row, 6, "plan_kind")?;
    let plan_kind = PlanKind::parse_str(&plan_kind_raw).ok_or_else(|| {
        StoreError::Backend(format!(
            "unknown plan kind in resolver row: {plan_kind_raw}"
        ))
    })?;
    Ok(ResolvedPlanHeader {
        assignment_id: row.first().and_then(|v| v.clone()),
        tenant_id: row.get(1).and_then(|v| v.clone()),
        guild_id: require_store_column(row, 2, "guild_id")?,
        plan_id: require_store_column(row, 3, "plan_id")?,
        plan_code: require_store_column(row, 4, "plan_code")?,
        plan_name: require_store_column(row, 5, "plan_name")?,
        plan_kind,
        resolution_source: require_store_column(row, 7, "resolution_source")?,
        assignment_source: row.get(8).and_then(|v| v.clone()),
        period_anchor: parse_optional_plan_timestamp(
            row.get(9).and_then(|v| v.clone()),
            "period_anchor",
        )?,
        valid_from: parse_optional_plan_timestamp(
            row.get(10).and_then(|v| v.clone()),
            "valid_from",
        )?,
        valid_until: parse_optional_plan_timestamp(
            row.get(11).and_then(|v| v.clone()),
            "valid_until",
        )?,
    })
}

fn parse_resolved_plan_quota_row(row: &SqlRow) -> Result<Option<PlanQuota>, StoreError> {
    let Some(quota_id) = row.get(12).and_then(|v| v.clone()) else {
        return Ok(None);
    };
    let dimension_raw = require_store_column(row, 13, "quota_dimension")?;
    let dimension = QuotaDimension::parse_str(&dimension_raw).ok_or_else(|| {
        StoreError::Backend(format!(
            "unknown quota dimension in resolver row: {dimension_raw}"
        ))
    })?;
    let period_raw = require_store_column(row, 14, "quota_period")?;
    let period = QuotaPeriod::parse_str(&period_raw).ok_or_else(|| {
        StoreError::Backend(format!(
            "unknown quota period in resolver row: {period_raw}"
        ))
    })?;
    let limit_value = optional_i64_column(row, 15, "quota_limit_value")?;
    let unlimited = required_bool_column(row, 16, "quota_unlimited")?;
    let enforcement_mode_raw = require_store_column(row, 17, "quota_enforcement_mode")?;
    let enforcement_mode =
        QuotaEnforcementMode::parse_str(&enforcement_mode_raw).ok_or_else(|| {
            StoreError::Backend(format!(
                "unknown quota enforcement mode in resolver row: {enforcement_mode_raw}"
            ))
        })?;
    PlanQuota::from_parts(
        quota_id,
        dimension,
        period,
        unlimited,
        limit_value,
        enforcement_mode,
    )
    .map(Some)
    .map_err(StoreError::Backend)
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

fn optional_i64_column(row: &SqlRow, idx: usize, field: &str) -> Result<Option<i64>, StoreError> {
    let Some(raw) = row.get(idx).and_then(|v| v.clone()) else {
        return Ok(None);
    };
    raw.parse::<i64>()
        .map(Some)
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn required_u64_column(row: &SqlRow, idx: usize, field: &str) -> Result<u64, StoreError> {
    let raw = require_store_column(row, idx, field)?;
    raw.parse::<u64>()
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn required_i64_column(row: &SqlRow, idx: usize, field: &str) -> Result<i64, StoreError> {
    let raw = require_store_column(row, idx, field)?;
    raw.parse::<i64>()
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

fn new_domain_knowledge_params(item: &NewDomainKnowledgeItem) -> Vec<String> {
    vec![
        item.id.clone(),
        item.guild_id.clone(),
        item.content_type.as_str().to_owned(),
        item.title.clone(),
        item.body.clone(),
        item.active.to_string(),
        item.updated_actor_user_id.clone().unwrap_or_default(),
    ]
}

fn update_domain_knowledge_params(item: &UpdateDomainKnowledgeItem) -> Vec<String> {
    vec![
        item.id.clone(),
        item.guild_id.clone(),
        item.content_type.as_str().to_owned(),
        item.title.clone(),
        item.body.clone(),
        item.active.to_string(),
        item.updated_actor_user_id.clone().unwrap_or_default(),
    ]
}

fn parse_domain_knowledge_row(row: &SqlRow) -> Result<DomainKnowledgeItem, StoreError> {
    if row.len() < 13 {
        return Err(StoreError::Backend(format!(
            "invalid domain knowledge row length: {}",
            row.len()
        )));
    }
    let content_type_raw = require_store_column(row, 3, "content_type")?;
    let content_type =
        DomainKnowledgeContentType::parse_str(&content_type_raw).ok_or_else(|| {
            StoreError::Backend(format!(
                "invalid domain knowledge content_type '{content_type_raw}'"
            ))
        })?;
    let created_at_raw = require_store_column(row, 11, "created_at")?;
    let updated_at_raw = require_store_column(row, 12, "updated_at")?;

    Ok(DomainKnowledgeItem {
        id: require_store_column(row, 0, "id")?,
        tenant_id: row.get(1).and_then(|v| v.clone()),
        guild_id: require_store_column(row, 2, "guild_id")?,
        content_type,
        title: require_store_column(row, 4, "title")?,
        body: require_store_column(row, 5, "body")?,
        active: required_bool_column(row, 6, "active")?,
        version: required_u32_column(row, 7, "version")?,
        updated_actor_user_id: row.get(8).and_then(|v| v.clone()),
        archived_at: parse_optional_domain_knowledge_timestamp(
            row.get(9).and_then(|v| v.clone()),
            "archived_at",
        )?,
        archived_actor_user_id: row.get(10).and_then(|v| v.clone()),
        created_at: parse_required_domain_knowledge_timestamp(&created_at_raw, "created_at")?,
        updated_at: parse_required_domain_knowledge_timestamp(&updated_at_raw, "updated_at")?,
    })
}

fn parse_required_domain_knowledge_timestamp(
    raw: &str,
    field: &str,
) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|err| {
            StoreError::Backend(format!("invalid domain knowledge {field} '{raw}': {err}"))
        })
}

fn parse_optional_domain_knowledge_timestamp(
    raw: Option<String>,
    field: &str,
) -> Result<Option<DateTime<Utc>>, StoreError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    parse_required_domain_knowledge_timestamp(&raw, field).map(Some)
}

fn postgres_text_array_literal(values: &[String]) -> String {
    let escaped = values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{escaped}}}")
}

fn parse_text_array_projection(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(',').map(str::to_owned).collect()
}

fn new_ai_memory_note_params(item: &NewAiMemoryNote) -> Vec<String> {
    vec![
        item.id.clone(),
        item.tenant_discord_guild_id.clone(),
        item.tenant_id.clone(),
        item.guild_id.clone(),
        item.title.clone(),
        item.body.clone(),
        postgres_text_array_literal(&ai_memory_tag_param_values(&item.tags)),
        item.source_type.as_str().to_owned(),
        item.source_meeting_id.clone().unwrap_or_default(),
        item.source_feedback_id.clone().unwrap_or_default(),
        item.confidence
            .map(ConfidencePermille::as_sql_decimal)
            .unwrap_or_default(),
        item.active.to_string(),
        item.pinned.to_string(),
        item.actor_user_id.clone(),
    ]
}

fn update_ai_memory_note_params(item: &UpdateAiMemoryNote) -> Vec<String> {
    vec![
        item.id.clone(),
        item.tenant_id.clone(),
        item.guild_id.clone(),
        item.title.clone(),
        item.body.clone(),
        postgres_text_array_literal(&ai_memory_tag_param_values(&item.tags)),
        item.confidence
            .map(ConfidencePermille::as_sql_decimal)
            .unwrap_or_default(),
        item.active.to_string(),
        item.pinned.to_string(),
        item.actor_user_id.clone(),
    ]
}

fn ai_memory_tag_param_values(tags: &[AiMemoryTag]) -> Vec<String> {
    tags.iter().map(|tag| tag.as_str().to_owned()).collect()
}

fn parse_ai_memory_note_row(row: &SqlRow) -> Result<AiMemoryNote, StoreError> {
    if row.len() < 20 {
        return Err(StoreError::Backend(format!(
            "invalid ai memory row length: {}",
            row.len()
        )));
    }
    let source_type_raw = require_store_column(row, 7, "source_type")?;
    let source_type = AiMemorySourceType::parse_str(&source_type_raw).ok_or_else(|| {
        StoreError::Backend(format!("invalid ai memory source_type '{source_type_raw}'"))
    })?;
    let tags_raw = require_store_column(row, 6, "tags")?;
    let tags = parse_text_array_projection(&tags_raw)
        .into_iter()
        .map(|tag| {
            AiMemoryTag::parse_str(&tag)
                .ok_or_else(|| StoreError::Backend(format!("invalid ai memory tag '{tag}'")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AiMemoryNote {
        id: require_store_column(row, 0, "id")?,
        tenant_discord_guild_id: require_store_column(row, 1, "tenant_discord_guild_id")?,
        tenant_id: require_store_column(row, 2, "tenant_id")?,
        guild_id: require_store_column(row, 3, "guild_id")?,
        title: require_store_column(row, 4, "title")?,
        body: require_store_column(row, 5, "body")?,
        tags,
        source_type,
        source_meeting_id: row.get(8).and_then(|v| v.clone()),
        source_feedback_id: row.get(9).and_then(|v| v.clone()),
        confidence: parse_optional_confidence(row.get(10).and_then(|v| v.clone()), "confidence")?,
        active: required_bool_column(row, 11, "active")?,
        pinned: required_bool_column(row, 12, "pinned")?,
        created_actor_user_id: require_store_column(row, 13, "created_actor_user_id")?,
        updated_actor_user_id: require_store_column(row, 14, "updated_actor_user_id")?,
        last_used_at: parse_optional_domain_knowledge_timestamp(
            row.get(15).and_then(|v| v.clone()),
            "last_used_at",
        )?,
        created_at: parse_required_domain_knowledge_timestamp(
            &require_store_column(row, 16, "created_at")?,
            "created_at",
        )?,
        updated_at: parse_required_domain_knowledge_timestamp(
            &require_store_column(row, 17, "updated_at")?,
            "updated_at",
        )?,
        archived_at: parse_optional_domain_knowledge_timestamp(
            row.get(18).and_then(|v| v.clone()),
            "archived_at",
        )?,
        archived_actor_user_id: row.get(19).and_then(|v| v.clone()),
    })
}

fn parse_optional_confidence(
    raw: Option<String>,
    field: &str,
) -> Result<Option<ConfidencePermille>, StoreError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    ConfidencePermille::parse_sql_decimal(&raw)
        .map(Some)
        .map_err(|err| StoreError::Backend(format!("invalid {field} '{raw}': {err}")))
}

fn new_transcript_feedback_params(item: &NewTranscriptFeedback) -> Vec<String> {
    vec![
        item.id.clone(),
        item.tenant_discord_guild_id.clone(),
        item.tenant_id.clone(),
        item.guild_id.clone(),
        item.meeting_id.clone().unwrap_or_default(),
        item.transcript_segment_id.clone().unwrap_or_default(),
        item.feedback_type.as_str().to_owned(),
        item.term_type
            .map(|term_type| term_type.as_str().to_owned())
            .unwrap_or_default(),
        item.original_text.clone().unwrap_or_default(),
        item.corrected_text.clone().unwrap_or_default(),
        item.speaker_id.clone().unwrap_or_default(),
        item.corrected_speaker_id.clone().unwrap_or_default(),
        item.note.clone().unwrap_or_default(),
        item.target_domain_knowledge_id.clone().unwrap_or_default(),
        item.target_ai_memory_note_id.clone().unwrap_or_default(),
        item.actor_user_id.clone(),
    ]
}

fn update_transcript_feedback_status_params(item: &UpdateTranscriptFeedbackStatus) -> Vec<String> {
    vec![
        item.id.clone(),
        item.tenant_id.clone(),
        item.guild_id.clone(),
        item.status.as_str().to_owned(),
        item.target_domain_knowledge_id.clone().unwrap_or_default(),
        item.target_ai_memory_note_id.clone().unwrap_or_default(),
        item.reviewed_actor_user_id.clone(),
    ]
}

fn parse_transcript_feedback_row(row: &SqlRow) -> Result<TranscriptFeedback, StoreError> {
    if row.len() < 20 {
        return Err(StoreError::Backend(format!(
            "invalid transcript feedback row length: {}",
            row.len()
        )));
    }
    let feedback_type_raw = require_store_column(row, 6, "feedback_type")?;
    let feedback_type = TranscriptFeedbackType::parse_str(&feedback_type_raw).ok_or_else(|| {
        StoreError::Backend(format!(
            "invalid transcript feedback feedback_type '{feedback_type_raw}'"
        ))
    })?;
    let term_type = row
        .get(7)
        .and_then(|v| v.clone())
        .map(|raw| {
            TranscriptFeedbackTermType::parse_str(&raw).ok_or_else(|| {
                StoreError::Backend(format!("invalid transcript feedback term_type '{raw}'"))
            })
        })
        .transpose()?;
    let status_raw = require_store_column(row, 16, "status")?;
    let status = TranscriptFeedbackStatus::parse_str(&status_raw).ok_or_else(|| {
        StoreError::Backend(format!("invalid transcript feedback status '{status_raw}'"))
    })?;
    Ok(TranscriptFeedback {
        id: require_store_column(row, 0, "id")?,
        tenant_discord_guild_id: require_store_column(row, 1, "tenant_discord_guild_id")?,
        tenant_id: require_store_column(row, 2, "tenant_id")?,
        guild_id: require_store_column(row, 3, "guild_id")?,
        meeting_id: row.get(4).and_then(|v| v.clone()),
        transcript_segment_id: row.get(5).and_then(|v| v.clone()),
        feedback_type,
        term_type,
        original_text: row.get(8).and_then(|v| v.clone()),
        corrected_text: row.get(9).and_then(|v| v.clone()),
        speaker_id: row.get(10).and_then(|v| v.clone()),
        corrected_speaker_id: row.get(11).and_then(|v| v.clone()),
        note: row.get(12).and_then(|v| v.clone()),
        target_domain_knowledge_id: row.get(13).and_then(|v| v.clone()),
        target_ai_memory_note_id: row.get(14).and_then(|v| v.clone()),
        actor_user_id: require_store_column(row, 15, "actor_user_id")?,
        status,
        created_at: parse_required_domain_knowledge_timestamp(
            &require_store_column(row, 17, "created_at")?,
            "created_at",
        )?,
        reviewed_at: parse_optional_domain_knowledge_timestamp(
            row.get(18).and_then(|v| v.clone()),
            "reviewed_at",
        )?,
        reviewed_actor_user_id: row.get(19).and_then(|v| v.clone()),
    })
}

fn new_person_alias_params(item: &NewPersonAlias) -> Vec<String> {
    vec![
        item.id.clone(),
        item.tenant_discord_guild_id.clone(),
        item.tenant_id.clone(),
        item.guild_id.clone(),
        item.canonical_name.clone(),
        item.alias.clone(),
        item.discord_user_id.clone().unwrap_or_default(),
        item.source_type.as_str().to_owned(),
        item.source_meeting_id.clone().unwrap_or_default(),
        item.source_feedback_id.clone().unwrap_or_default(),
        item.confidence
            .map(ConfidencePermille::as_sql_decimal)
            .unwrap_or_default(),
        item.active.to_string(),
        item.review_status.as_str().to_owned(),
        item.actor_user_id.clone(),
    ]
}

fn update_person_alias_params(item: &UpdatePersonAlias) -> Vec<String> {
    vec![
        item.id.clone(),
        item.tenant_id.clone(),
        item.guild_id.clone(),
        item.canonical_name.clone(),
        item.alias.clone(),
        item.discord_user_id.clone().unwrap_or_default(),
        item.confidence
            .map(ConfidencePermille::as_sql_decimal)
            .unwrap_or_default(),
        item.active.to_string(),
        item.review_status.as_str().to_owned(),
        item.actor_user_id.clone(),
    ]
}

fn parse_person_alias_row(row: &SqlRow) -> Result<PersonAlias, StoreError> {
    if row.len() < 21 {
        return Err(StoreError::Backend(format!(
            "invalid person alias row length: {}",
            row.len()
        )));
    }
    let source_type_raw = require_store_column(row, 7, "source_type")?;
    let source_type = PersonAliasSourceType::parse_str(&source_type_raw).ok_or_else(|| {
        StoreError::Backend(format!(
            "invalid person alias source_type '{source_type_raw}'"
        ))
    })?;
    let review_status_raw = require_store_column(row, 12, "review_status")?;
    let review_status =
        PersonAliasReviewStatus::parse_str(&review_status_raw).ok_or_else(|| {
            StoreError::Backend(format!(
                "invalid person alias review_status '{review_status_raw}'"
            ))
        })?;
    Ok(PersonAlias {
        id: require_store_column(row, 0, "id")?,
        tenant_discord_guild_id: require_store_column(row, 1, "tenant_discord_guild_id")?,
        tenant_id: require_store_column(row, 2, "tenant_id")?,
        guild_id: require_store_column(row, 3, "guild_id")?,
        canonical_name: require_store_column(row, 4, "canonical_name")?,
        alias: require_store_column(row, 5, "alias")?,
        discord_user_id: row.get(6).and_then(|v| v.clone()),
        source_type,
        source_meeting_id: row.get(8).and_then(|v| v.clone()),
        source_feedback_id: row.get(9).and_then(|v| v.clone()),
        confidence: parse_optional_confidence(row.get(10).and_then(|v| v.clone()), "confidence")?,
        active: required_bool_column(row, 11, "active")?,
        review_status,
        created_actor_user_id: require_store_column(row, 13, "created_actor_user_id")?,
        updated_actor_user_id: require_store_column(row, 14, "updated_actor_user_id")?,
        reviewed_at: parse_optional_domain_knowledge_timestamp(
            row.get(15).and_then(|v| v.clone()),
            "reviewed_at",
        )?,
        reviewed_actor_user_id: row.get(16).and_then(|v| v.clone()),
        archived_at: parse_optional_domain_knowledge_timestamp(
            row.get(17).and_then(|v| v.clone()),
            "archived_at",
        )?,
        archived_actor_user_id: row.get(18).and_then(|v| v.clone()),
        created_at: parse_required_domain_knowledge_timestamp(
            &require_store_column(row, 19, "created_at")?,
            "created_at",
        )?,
        updated_at: parse_required_domain_knowledge_timestamp(
            &require_store_column(row, 20, "updated_at")?,
            "updated_at",
        )?,
    })
}

fn new_summary_template_params(item: &NewSummaryTemplate) -> Vec<String> {
    vec![
        item.id.clone(),
        item.guild_id.clone(),
        item.name.clone(),
        item.template.clone(),
        item.active.to_string(),
        item.updated_actor_user_id.clone().unwrap_or_default(),
    ]
}

fn update_summary_template_params(item: &UpdateSummaryTemplate) -> Vec<String> {
    vec![
        item.id.clone(),
        item.guild_id.clone(),
        item.name.clone(),
        item.template.clone(),
        item.active
            .map(|active| active.to_string())
            .unwrap_or_default(),
        item.updated_actor_user_id.clone().unwrap_or_default(),
    ]
}

fn parse_summary_template_row(row: &SqlRow) -> Result<SummaryTemplate, StoreError> {
    if row.len() < 12 {
        return Err(StoreError::Backend(format!(
            "invalid summary template row length: {}",
            row.len()
        )));
    }
    let created_at_raw = require_store_column(row, 10, "created_at")?;
    let updated_at_raw = require_store_column(row, 11, "updated_at")?;

    Ok(SummaryTemplate {
        id: require_store_column(row, 0, "id")?,
        tenant_id: row.get(1).and_then(|v| v.clone()),
        guild_id: require_store_column(row, 2, "guild_id")?,
        name: require_store_column(row, 3, "name")?,
        template: require_store_column(row, 4, "template")?,
        active: required_bool_column(row, 5, "active")?,
        version: required_u32_column(row, 6, "version")?,
        updated_actor_user_id: row.get(7).and_then(|v| v.clone()),
        archived_at: parse_optional_domain_knowledge_timestamp(
            row.get(8).and_then(|v| v.clone()),
            "archived_at",
        )?,
        archived_actor_user_id: row.get(9).and_then(|v| v.clone()),
        created_at: parse_required_domain_knowledge_timestamp(&created_at_raw, "created_at")?,
        updated_at: parse_required_domain_knowledge_timestamp(&updated_at_raw, "updated_at")?,
    })
}

pub fn audit_event_params(event: &AuditEvent) -> Vec<String> {
    vec![
        event.id.clone(),
        event.tenant_id.clone().unwrap_or_default(),
        event.guild_id.clone().unwrap_or_default(),
        event.actor_user_id.clone().unwrap_or_default(),
        event.action.clone(),
        event.resource_type.clone(),
        event.resource_id.clone().unwrap_or_default(),
        event.request_metadata_json.clone(),
        event.detail_json.as_str().to_owned(),
        event.occurred_at.to_rfc3339(),
    ]
}

pub fn usage_event_params(event: &NewUsageEvent) -> Vec<String> {
    vec![
        event.id.clone(),
        event.tenant_id.clone().unwrap_or_default(),
        event.guild_id.clone(),
        event.meeting_id.clone().unwrap_or_default(),
        event.job_id.clone().unwrap_or_default(),
        event.resource_type.clone().unwrap_or_default(),
        event.resource_id.clone().unwrap_or_default(),
        event.metric.as_str().to_owned(),
        event.quantity.to_string(),
        event.detail_json.as_str().to_owned(),
        event.observed_at.to_rfc3339(),
    ]
}

fn parse_audit_event_row(row: &SqlRow) -> Result<AuditEvent, StoreError> {
    if row.len() < 11 {
        return Err(StoreError::Backend(format!(
            "invalid audit event row length: {}",
            row.len()
        )));
    }
    let occurred_at_raw = require_store_column(row, 9, "occurred_at")?;
    let created_at_raw = require_store_column(row, 10, "created_at")?;
    Ok(AuditEvent {
        id: require_store_column(row, 0, "id")?,
        tenant_id: row.get(1).and_then(|v| v.clone()),
        guild_id: row.get(2).and_then(|v| v.clone()),
        actor_user_id: row.get(3).and_then(|v| v.clone()),
        action: require_store_column(row, 4, "action")?,
        resource_type: require_store_column(row, 5, "resource_type")?,
        resource_id: row.get(6).and_then(|v| v.clone()),
        request_metadata_json: require_store_column(row, 7, "request_metadata")?,
        detail_json: require_store_column(row, 8, "detail_json")?,
        occurred_at: DateTime::parse_from_rfc3339(&occurred_at_raw)
            .map(|ts| ts.with_timezone(&Utc))
            .map_err(|err| {
                StoreError::Backend(format!(
                    "invalid audit occurred_at '{occurred_at_raw}': {err}"
                ))
            })?,
        created_at: DateTime::parse_from_rfc3339(&created_at_raw)
            .map(|ts| ts.with_timezone(&Utc))
            .map_err(|err| {
                StoreError::Backend(format!(
                    "invalid audit created_at '{created_at_raw}': {err}"
                ))
            })?,
    })
}

fn parse_usage_event_row(row: &SqlRow) -> Result<UsageEvent, StoreError> {
    if row.len() < 12 {
        return Err(StoreError::Backend(format!(
            "invalid usage event row length: {}",
            row.len()
        )));
    }
    let metric_raw = require_store_column(row, 7, "metric")?;
    let metric = UsageMetric::parse_str(&metric_raw)
        .ok_or_else(|| StoreError::Backend(format!("unknown usage metric: {metric_raw}")))?;
    let observed_at_raw = require_store_column(row, 10, "observed_at")?;
    let created_at_raw = require_store_column(row, 11, "created_at")?;

    Ok(UsageEvent {
        id: require_store_column(row, 0, "id")?,
        tenant_id: row.get(1).and_then(|v| v.clone()),
        guild_id: require_store_column(row, 2, "guild_id")?,
        meeting_id: row.get(3).and_then(|v| v.clone()),
        job_id: row.get(4).and_then(|v| v.clone()),
        resource_type: row.get(5).and_then(|v| v.clone()),
        resource_id: row.get(6).and_then(|v| v.clone()),
        metric,
        quantity: required_i64_column(row, 8, "quantity")?,
        detail_json: require_store_column(row, 9, "detail_json")?,
        observed_at: parse_required_usage_timestamp(&observed_at_raw, "observed_at")?,
        created_at: parse_required_usage_timestamp(&created_at_raw, "created_at")?,
    })
}

fn parse_usage_aggregate_row(row: &SqlRow) -> Result<UsageAggregate, StoreError> {
    if row.len() < 2 {
        return Err(StoreError::Backend(format!(
            "invalid usage aggregate row length: {}",
            row.len()
        )));
    }
    let metric_raw = require_store_column(row, 0, "metric")?;
    let metric = UsageMetric::parse_str(&metric_raw)
        .ok_or_else(|| StoreError::Backend(format!("unknown usage metric: {metric_raw}")))?;
    Ok(UsageAggregate {
        metric,
        quantity: required_i64_column(row, 1, "quantity")?,
    })
}

fn parse_required_usage_timestamp(raw: &str, field: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|err| StoreError::Backend(format!("invalid usage {field} '{raw}': {err}")))
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

fn parse_optional_plan_timestamp(
    value: Option<String>,
    field: &str,
) -> Result<Option<DateTime<Utc>>, StoreError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(&raw)
        .map(|ts| Some(ts.with_timezone(&Utc)))
        .map_err(|err| StoreError::Backend(format!("invalid plan {field} '{raw}': {err}")))
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
                "SELECT id, guild_id, voice_channel_id, voice_channel_name, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, \
                        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
                        to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as stopped_at, \
                        meeting_duration_seconds \
                  FROM meetings WHERE id=$1 LIMIT 1",
                &[meeting_id.to_owned()],
            )
            .map_err(StoreError::Backend)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        sql_row_to_stored_meeting(&row, &format!("id={meeting_id}")).map(Some)
    }

    fn create_scheduled_meeting(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError> {
        let meeting_id = request.id.clone();
        let guild_id = request.guild_id.clone();
        if let Some(settings) = request.effective_settings.clone() {
            let params = create_meeting_with_settings_params(request, settings);
            let result = self.executor.execute(
                INSERT_SCHEDULED_MEETING_WITH_EFFECTIVE_SETTINGS_SQL,
                &params,
            );
            if let Err(err) = result {
                return Err(self.map_active_meeting_insert_error(err, meeting_id, guild_id));
            }
        } else {
            let sql = "INSERT INTO meetings(id,guild_id,voice_channel_id,voice_channel_name,report_channel_id,status_message_channel_id,status_message_id,started_by_user_id,status) VALUES($1,$2,$3,NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,'scheduled')";
            let result = self.executor.execute(
                sql,
                &[
                    request.id,
                    request.guild_id,
                    request.voice_channel_id,
                    request.voice_channel_name.unwrap_or_default(),
                    request.report_channel_id,
                    request.status_message_channel_id.unwrap_or_default(),
                    request.status_message_id.unwrap_or_default(),
                    request.started_by_user_id,
                ],
            );
            if let Err(err) = result {
                return Err(self.map_active_meeting_insert_error(err, meeting_id, guild_id));
            }
        }
        Ok(())
    }

    fn create_meeting_as_recording(
        &mut self,
        request: CreateMeetingRequest,
    ) -> Result<(), StoreError> {
        let meeting_id = request.id.clone();
        let guild_id = request.guild_id.clone();
        if let Some(settings) = request.effective_settings.clone() {
            let params = create_meeting_with_settings_params(request, settings);
            let result = self.executor.execute(
                INSERT_RECORDING_MEETING_WITH_EFFECTIVE_SETTINGS_SQL,
                &params,
            );
            if let Err(err) = result {
                return Err(self.map_active_meeting_insert_error(err, meeting_id, guild_id));
            }
        } else {
            let sql = "INSERT INTO meetings(id,guild_id,voice_channel_id,voice_channel_name,report_channel_id,status_message_channel_id,status_message_id,started_by_user_id,status) VALUES($1,$2,$3,NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,'recording')";
            let result = self.executor.execute(
                sql,
                &[
                    request.id,
                    request.guild_id,
                    request.voice_channel_id,
                    request.voice_channel_name.unwrap_or_default(),
                    request.report_channel_id,
                    request.status_message_channel_id.unwrap_or_default(),
                    request.status_message_id.unwrap_or_default(),
                    request.started_by_user_id,
                ],
            );
            if let Err(err) = result {
                return Err(self.map_active_meeting_insert_error(err, meeting_id, guild_id));
            }
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

    fn set_meeting_title(&mut self, meeting_id: &str, title: String) -> Result<(), StoreError> {
        let affected = self
            .executor
            .execute(
                "UPDATE meetings SET title=$1, updated_at=NOW() WHERE id=$2",
                &[title, meeting_id.to_owned()],
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
        request.voice_channel_name.unwrap_or_default(),
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
                        let constraint = err
                            .as_db_error()
                            .and_then(|db_err| db_err.constraint())
                            .unwrap_or("");
                        format!("{UNIQUE_VIOLATION_PREFIX}{constraint}: {err}")
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
        let sql = "SELECT id, guild_id, voice_channel_id, voice_channel_name, report_channel_id, status_message_channel_id, status_message_id, started_by_user_id, title, status, stop_reason, error_message, to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS started_at, to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS stopped_at, meeting_duration_seconds FROM meetings WHERE guild_id=$1 AND status IN ('scheduled','recording','stopping') ORDER BY started_at DESC LIMIT 1";
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
        voice_channel_name: row.get("voice_channel_name"),
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
        duration_seconds: row
            .try_get::<_, Option<i32>>("meeting_duration_seconds")
            .ok()
            .flatten()
            .and_then(|value| u64::try_from(value).ok()),
    })
}

fn sql_row_to_stored_meeting(row: &SqlRow, context: &str) -> Result<StoredMeeting, StoreError> {
    if row.len() < 15 {
        return Err(StoreError::Backend(format!(
            "invalid meeting row length for {context}: {}",
            row.len()
        )));
    }
    let require = |idx: usize, field: &str| -> Result<String, StoreError> {
        row.get(idx)
            .and_then(|v| v.clone())
            .ok_or_else(|| StoreError::Backend(format!("{field} is NULL for {context}")))
    };
    let meeting_id = require(0, "id")?;
    let status_raw = require(9, "status")?;
    let status = MeetingStatus::parse_str(&status_raw).ok_or_else(|| {
        StoreError::Backend(format!(
            "invalid meeting status for meeting_id={meeting_id}: {status_raw}"
        ))
    })?;
    let stop_reason = parse_stop_reason_column(
        row.get(10).and_then(|v| v.clone()),
        &format!("meeting_id={meeting_id}"),
    )
    .map_err(StoreError::Backend)?;

    Ok(StoredMeeting {
        id: meeting_id,
        guild_id: require(1, "guild_id")?,
        voice_channel_id: require(2, "voice_channel_id")?,
        voice_channel_name: row.get(3).and_then(|v| v.clone()),
        report_channel_id: require(4, "report_channel_id")?,
        status_message_channel_id: row.get(5).and_then(|v| v.clone()),
        status_message_id: row.get(6).and_then(|v| v.clone()),
        started_by_user_id: require(7, "started_by_user_id")?,
        title: row.get(8).and_then(|v| v.clone()),
        status,
        stop_reason,
        error_message: row.get(11).and_then(|v| v.clone()),
        started_at: parse_optional_rfc3339(row.get(12).and_then(|v| v.clone())),
        stopped_at: parse_optional_rfc3339(row.get(13).and_then(|v| v.clone())),
        duration_seconds: parse_optional_u64(row.get(14).and_then(|v| v.clone())),
    })
}

fn parse_optional_rfc3339(value: Option<String>) -> Option<DateTime<Utc>> {
    value
        .as_deref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|ts| ts.with_timezone(&Utc))
}

fn parse_optional_u64(value: Option<String>) -> Option<u64> {
    value.and_then(|value| value.parse::<u64>().ok())
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
        if let Ok(v) = row.try_get::<usize, Option<i32>>(idx) {
            values.push(v.map(|value| value.to_string()));
            continue;
        }
        if let Ok(v) = row.try_get::<usize, i32>(idx) {
            values.push(Some(v.to_string()));
            continue;
        }
        if let Ok(v) = row.try_get::<usize, Option<i64>>(idx) {
            values.push(v.map(|value| value.to_string()));
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
