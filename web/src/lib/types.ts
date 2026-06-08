export interface MeetingResponse {
  id: string;
  title: string | null;
  status: string;
  started_at: string | null;
  stopped_at: string | null;
  duration_seconds: number | null;
  voice_channel_id: string;
  voice_channel_name: string | null;
}

export interface MeResponse {
  user_id: string;
  // Returned by /api/me for future guild-scoped UI decisions.
  guild_id: string;
  is_admin: boolean;
  can_manage_settings: boolean;
  can_view_admin?: boolean;
  can_view_usage: boolean;
  can_reprocess_meetings: boolean;
  can_manage_domain_knowledge: boolean;
  can_manage_summary_templates: boolean;
}

export interface UserGuild {
  guild_id: string;
  name: string;
  icon: string | null;
  is_member: boolean;
  is_admin: boolean;
  installed: boolean;
}

export interface MeetingListItem {
  id: string;
  title: string | null;
  status: string;
  started_at: string | null;
  stopped_at: string | null;
  duration_seconds: number | null;
  stop_reason: string | null;
  voice_channel_id: string;
  voice_channel_name: string | null;
}

export interface MeetingVoiceChannel {
  id: string;
  label: string;
}

export interface MeetingListResponse {
  guild_id: string;
  meetings: MeetingListItem[];
  voice_channels: MeetingVoiceChannel[];
  page: number;
  limit: number;
  total: number;
}

export type GuildJobStatus =
  | "queued"
  | "running"
  | "failed"
  | "done"
  | "canceled";
export type GuildJobType = "transcribe" | "summarize" | "cleanup";

export interface GuildJob {
  id: string;
  meeting_id: string;
  guild_id: string;
  job_type: GuildJobType;
  status: GuildJobStatus;
  retry_count: number;
  error_message: string | null;
  next_run_at: string | null;
  leased_until: string | null;
  finished_at: string | null;
  dead_lettered_at: string | null;
  canceled_at: string | null;
  cancel_reason: string | null;
  created_at: string;
  updated_at: string;
}

export interface GuildSettingsResponse {
  whisper_language: string | null;
  whisper_language_explicit: boolean;
  whisper_vad: boolean;
  auto_stop_grace_seconds: number;
  retention_raw_audio_ttl_days: number;
  retention_transcript_ttl_days: number;
  summary_enabled: boolean;
  discord_bot_token_registered: boolean;
  discord_bot_token_updated_at: string | null;
  discord_bot_token_last_validated_at: string | null;
  discord_bot_user_id: string | null;
  discord_bot_username: string | null;
  is_admin: boolean;
  can_manage_settings: boolean;
  can_manage_domain_knowledge: boolean;
  can_manage_summary_templates: boolean;
}

export interface UpdateGuildSettingsRequest {
  whisper_language: string | null;
  whisper_vad: boolean;
  auto_stop_grace_seconds: number;
  retention_raw_audio_ttl_days: number;
  retention_transcript_ttl_days: number;
  summary_enabled: boolean;
}

export interface UpdateGuildBotTokenRequest {
  bot_token: string;
}

export interface GuildRbacPermissionCatalogEntry {
  name: string;
  label: string;
  description: string;
}

export interface GuildRbacRoleGrant {
  discord_role_id: string;
  permissions: string[];
  created_actor_user_id: string | null;
  updated_actor_user_id: string | null;
  created_at: string | null;
  updated_at: string | null;
}

export interface GuildRbacRole {
  id: string;
  name: string;
  position: number;
  color: number;
  managed: boolean;
  hoist: boolean;
  is_admin: boolean;
  grant: GuildRbacRoleGrant | null;
}

export interface GuildRbacManagement {
  guild_id: string;
  permissions: GuildRbacPermissionCatalogEntry[];
  roles: GuildRbacRole[];
  degraded: boolean;
}

export interface GuildRbacRoleGrantUpdateRequest {
  permissions: string[];
}

export type AdminPlanKind = "default" | "beta" | "custom";
export type AdminPlanStatus = "active" | "archived";
export type AdminQuotaDimension =
  | "recording_minutes"
  | "asr_seconds"
  | "summary_runs"
  | "storage_bytes"
  | "debug_downloads";
export type AdminQuotaPeriod = "daily" | "monthly" | "total" | "current";
export type AdminQuotaEnforcementMode = "observe_only" | "enforce";
export type AdminGuildPlanAssignmentStatus = "active" | "revoked";
export type AdminGuildPlanAssignmentSource =
  | "system"
  | "admin"
  | "billing_provider"
  | "migration";

export interface AdminPlan {
  id: string;
  code: string;
  name: string;
  kind: AdminPlanKind;
  status: AdminPlanStatus;
  quotas: AdminPlanQuota[];
  created_at: string;
  updated_at: string;
}

export interface AdminPlanUpsertRequest {
  id?: string;
  code: string;
  name: string;
  kind: AdminPlanKind;
  status?: AdminPlanStatus;
}

export interface AdminPlanQuota {
  id: string;
  plan_id: string;
  dimension: AdminQuotaDimension;
  period: AdminQuotaPeriod;
  limit_value: number | null;
  unlimited: boolean;
  enforcement_mode: AdminQuotaEnforcementMode;
  created_at: string;
  updated_at: string;
}

export interface AdminPlanQuotaUpsertRequest {
  id?: string;
  dimension: AdminQuotaDimension;
  period: AdminQuotaPeriod;
  limit_value?: number | null;
  unlimited?: boolean;
  enforcement_mode: AdminQuotaEnforcementMode;
}

export interface AdminGuildPlanAssignment {
  id: string;
  tenant_id: string;
  guild_id: string;
  plan_id: string;
  plan_code: string;
  plan_name: string;
  status: AdminGuildPlanAssignmentStatus;
  valid_from: string;
  valid_until: string | null;
  period_anchor: string;
  assigned_by_user_id: string | null;
  source: AdminGuildPlanAssignmentSource;
  created_at: string;
  updated_at: string;
}

export interface AdminGuildPlanAssignmentUpsertRequest {
  id?: string;
  tenant_id?: string;
  guild_id?: string;
  plan_id: string;
  valid_from: string;
  valid_until?: string | null;
  assigned_by_user_id?: string | null;
  source: AdminGuildPlanAssignmentSource;
}

export interface AdminGuildPlanAssignmentCreateRequest
  extends AdminGuildPlanAssignmentUpsertRequest {
  tenant_id: string;
  guild_id: string;
}

export interface AdminRetentionPolicyRequest {
  raw_audio_ttl_days?: number;
  transcript_ttl_days?: number;
  summary_ttl_days?: number | null;
}

export interface AdminRetentionTargets {
  raw_audio: boolean;
  transcript: boolean;
  summary: boolean;
  debug: boolean;
}

export interface AdminRetentionStorageUsage {
  raw_audio_bytes: number;
  transcript_bytes: number;
  summary_bytes: number;
  debug_bytes: number;
  total_bytes: number;
}

export interface AdminRetentionPolicy {
  raw_audio_ttl_days: number;
  transcript_ttl_days: number;
  summary_ttl_days: number | null;
  debug_ttl_source: string;
}

export interface AdminRetentionLegalHold {
  supported: boolean;
  active: boolean;
  message: string;
}

export interface AdminRetentionQuotaReadiness {
  storage_bytes_observed: number;
  storage_bytes_current: number;
  enforcement_mode: string;
  hard_quota_enforced: boolean;
}

export interface AdminRetentionOverview {
  guild_id: string;
  policy: AdminRetentionPolicy;
  legal_hold: AdminRetentionLegalHold;
  storage: AdminRetentionStorageUsage;
  artifact_count: number;
  meeting_count: number;
  active_meeting_count: number;
  quota_readiness: AdminRetentionQuotaReadiness;
}

export interface AdminRetentionCleanupPreview {
  guild_id: string;
  policy: AdminRetentionPolicy;
  deletion_targets: AdminRetentionTargets;
  raw_workspace_count: number;
  transcript_workspace_count: number;
  summary_workspace_count: number;
  expired_artifact_count: number;
  expired_artifact_bytes: number;
  estimated_freed_bytes: AdminRetentionStorageUsage;
}

export interface AdminRetentionCleanupReport {
  raw_workspaces_scanned: number;
  raw_audio_dirs_removed: number;
  legacy_meetings_cleaned: number;
  raw_workspaces_marked_cleaned: number;
  speaker_dirs_removed: number;
  context_dirs_removed: number;
  transcript_dirs_removed: number;
  empty_summary_dirs_removed: number;
  summary_dirs_removed: number;
  debug_dirs_removed: number;
  agent_workspace_dirs_removed: number;
  transcripts_marked_deleted: number;
  summaries_deleted: number;
  artifacts_deleted: number;
}

export interface AdminRetentionCleanupRun {
  preview: AdminRetentionCleanupPreview;
  report: AdminRetentionCleanupReport;
  audit_recorded: boolean;
  error: string | null;
}

export interface AdminRetentionMeetingDeleteRequest {
  targets: AdminRetentionTargets;
  reason?: string | null;
}

export interface AdminRetentionMeetingDeletePreview {
  guild_id: string;
  meeting_id: string;
  voice_channel_id: string;
  status: string;
  started_at: string | null;
  stopped_at: string | null;
  targets: AdminRetentionTargets;
  storage: AdminRetentionStorageUsage;
  estimated_freed_bytes: AdminRetentionStorageUsage;
  transcript_count: number;
  summary_count: number;
  artifact_count: number;
  usage_event_count: number;
  audit_event_count: number;
  legal_hold: AdminRetentionLegalHold;
  preserves_usage_history: boolean;
  preserves_audit_history: boolean;
}

export interface AdminRetentionMeetingDelete {
  preview: AdminRetentionMeetingDeletePreview;
  report: AdminRetentionCleanupReport;
  audit_recorded: boolean;
  error: string | null;
}

export type DomainKnowledgeContentType =
  | "glossary"
  | "person_name"
  | "project_context"
  | "wording_rule"
  | "prohibited_item";

export interface DomainKnowledgeItem {
  id: string;
  content_type: DomainKnowledgeContentType;
  title: string;
  body: string;
  active: boolean;
  version: number;
  updated_actor_user_id: string | null;
  archived_at: string | null;
  archived_actor_user_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface DomainKnowledgeUpsertRequest {
  content_type: DomainKnowledgeContentType;
  title: string;
  body: string;
  active?: boolean;
}

export interface SummaryTemplate {
  id: string;
  name: string;
  template: string;
  active: boolean;
  version: number;
  updated_actor_user_id: string | null;
  archived_at: string | null;
  archived_actor_user_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface SummaryTemplateUpsertRequest {
  name: string;
  template: string;
  active?: boolean;
}

export type AiMemorySourceType =
  | "ai_meeting_extraction"
  | "user_feedback"
  | "manual"
  | "vc_participant"
  | "promotion_candidate";

export type AiMemoryTag =
  | "person"
  | "alias"
  | "project"
  | "product"
  | "terminology"
  | "decision"
  | "team_convention"
  | "summary_hint"
  | "transcription_hint"
  | "uncertain";

export interface AiMemoryNote {
  id: string;
  title: string;
  body: string;
  tags: AiMemoryTag[];
  source_type: AiMemorySourceType;
  source_meeting_id: string | null;
  source_feedback_id: string | null;
  confidence: number | null;
  active: boolean;
  pinned: boolean;
  last_used_at: string | null;
  archived_at: string | null;
  archived_actor_user_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface AiMemoryUpsertRequest {
  id?: string;
  title: string;
  body: string;
  tags?: AiMemoryTag[];
  source_type?: AiMemorySourceType;
  source_meeting_id?: string | null;
  source_feedback_id?: string | null;
  confidence?: number | null;
  active?: boolean;
  pinned?: boolean;
}

export interface AiMemoryPromoteRequest {
  content_type: DomainKnowledgeContentType;
}

export interface SpeakerResponse {
  id: string;
  username: string | null;
  nickname: string | null;
  display_name: string | null;
  display_label: string;
}

export interface TranscriptSegment {
  id?: string;
  speaker_id: string;
  speaker: SpeakerResponse;
  start_ms: number;
  end_ms: number;
  text: string;
  confidence: number | null;
  is_noisy: boolean;
  source: "voice" | "vc_text";
}

export interface TranscriptResponse {
  segments: TranscriptSegment[];
  status: string;
  is_final: boolean;
  updated_at: string | null;
}

export interface TranscriptStateResponse {
  status: string;
  is_final: boolean;
  updated_at: string | null;
}

export type TranscriptFeedbackType =
  | "mistranscription"
  | "speaker"
  | "term"
  | "person_alias"
  | "domain_knowledge"
  | "ai_memory";

export type TranscriptFeedbackTermType =
  | "general_term"
  | "person_name"
  | "project_name"
  | "product_name"
  | "organization"
  | "acronym"
  | "wording_rule"
  | "prohibited_item";

export interface TranscriptFeedbackRequest {
  transcript_segment_id?: string;
  feedback_type: TranscriptFeedbackType;
  term_type?: TranscriptFeedbackTermType;
  original_text?: string;
  corrected_text?: string;
  speaker_id?: string;
  corrected_speaker_id?: string;
  note?: string;
  target_domain_knowledge_id?: string;
  target_ai_memory_note_id?: string;
}

export interface TranscriptFeedbackResponse {
  id: string;
  meeting_id: string | null;
  transcript_segment_id: string | null;
  feedback_type: string;
  term_type: string | null;
  original_text: string | null;
  corrected_text: string | null;
  speaker_id: string | null;
  corrected_speaker_id: string | null;
  note: string | null;
  target_domain_knowledge_id: string | null;
  target_ai_memory_note_id: string | null;
  actor_user_id: string;
  status: string;
  created_at: string;
  reviewed_at: string | null;
  reviewed_actor_user_id: string | null;
}

export type TranscriptFeedbackStatus =
  | "open"
  | "accepted"
  | "dismissed"
  | "converted_to_domain_knowledge"
  | "converted_to_ai_memory";

export interface TranscriptFeedbackStatusRequest {
  status: Exclude<TranscriptFeedbackStatus, "open">;
  target_domain_knowledge_id?: string | null;
  target_ai_memory_note_id?: string | null;
}

export type PersonAliasSourceType =
  | "user_feedback"
  | "ai_inference"
  | "vc_participant"
  | "manual";

export type PersonAliasReviewStatus = "unreviewed" | "accepted" | "dismissed";

export interface PersonAlias {
  id: string;
  canonical_name: string;
  alias: string;
  discord_user_id: string | null;
  source_type: PersonAliasSourceType;
  source_meeting_id: string | null;
  source_feedback_id: string | null;
  confidence: number | null;
  active: boolean;
  review_status: PersonAliasReviewStatus;
  reviewed_at: string | null;
  reviewed_actor_user_id: string | null;
  archived_at: string | null;
  archived_actor_user_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface PersonAliasUpsertRequest {
  id?: string;
  canonical_name: string;
  alias: string;
  discord_user_id?: string | null;
  source_type?: PersonAliasSourceType;
  source_meeting_id?: string | null;
  source_feedback_id?: string | null;
  confidence?: number | null;
  active?: boolean;
  review_status?: PersonAliasReviewStatus;
}

export type TranscriptStreamState =
  | "idle"
  | "connecting"
  | "open"
  | "reconnecting"
  | "closed"
  | "error"
  | "forbidden";

export interface SummaryResponse {
  markdown: string | null;
}

export interface SpeakerAudioInfo {
  speaker_id: string;
  username: string | null;
  nickname: string | null;
  display_name: string | null;
  display_label: string;
  has_audio: boolean;
}

export type DebugCategory =
  | "audio"
  | "whisper"
  | "transcript"
  | "prompt"
  | "summary";

export interface DebugArtifact {
  id: string;
  label: string;
  category: DebugCategory;
  available: boolean;
  download_url: string;
  filename: string;
  content_type: string;
}
