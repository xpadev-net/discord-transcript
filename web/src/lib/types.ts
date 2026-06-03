export interface MeetingResponse {
  id: string;
  title: string | null;
  status: string;
  started_at: string | null;
  stopped_at: string | null;
  duration_seconds: number | null;
}

export interface MeResponse {
  user_id: string;
  // Returned by /api/me for future guild-scoped UI decisions.
  guild_id: string;
  is_admin: boolean;
}

export interface MeetingListItem {
  id: string;
  title: string | null;
  status: string;
  started_at: string | null;
  stopped_at: string | null;
  duration_seconds: number | null;
  stop_reason: string | null;
}

export interface MeetingListResponse {
  meetings: MeetingListItem[];
  page: number;
  limit: number;
  total: number;
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
