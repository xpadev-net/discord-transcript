export interface MeetingResponse {
  id: string;
  title: string | null;
  status: string;
  started_at: string | null;
  stopped_at: string | null;
  duration_seconds: number | null;
}

export interface SpeakerResponse {
  id: string;
  username: string | null;
  nickname: string | null;
  display_name: string | null;
  display_label: string;
}

export interface TranscriptSegment {
  speaker_id: string;
  speaker: SpeakerResponse;
  start_ms: number;
  end_ms: number;
  text: string;
  confidence: number | null;
  is_noisy: boolean;
  source: "voice" | "vc_text";
}

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

// Guild dashboard types

export interface MeResponse {
  user_id: string;
  is_guild_admin: boolean;
}

export interface MeetingListItem {
  id: string;
  status: string;
  title: string | null;
  started_at: string | null;
  stopped_at: string | null;
  duration_seconds: number | null;
}

export interface MeetingListResponse {
  meetings: MeetingListItem[];
  total: number;
  page: number;
  limit: number;
}

export interface GuildSettingsResponse {
  whisper_language: string | null;
  whisper_language_explicit: boolean;
  whisper_vad: boolean | null;
  auto_stop_grace_seconds: number | null;
  retention_raw_audio_ttl_days: number | null;
  retention_transcript_ttl_days: number | null;
  summary_enabled: boolean | null;
}

export interface UpdateGuildSettingsRequest {
  whisper_language?: string | null;
  whisper_language_explicit?: boolean;
  whisper_vad?: boolean;
  auto_stop_grace_seconds?: number;
  retention_raw_audio_ttl_days?: number;
  retention_transcript_ttl_days?: number;
  summary_enabled?: boolean;
}
