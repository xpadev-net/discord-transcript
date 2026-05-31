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
