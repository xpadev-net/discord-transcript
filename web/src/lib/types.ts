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
