import { getSpeakerAudioUrl } from "../lib/api";
import type { SpeakerAudioInfo } from "../lib/types";

interface SpeakerAudioDownloadsProps {
  meetingId: string;
  speakers: SpeakerAudioInfo[] | null;
  loading: boolean;
  error: boolean;
}

function sanitizeFilename(name: string): string {
  const sanitized = name
    .trim()
    .split("")
    .map((c) => {
      const cp = c.charCodeAt(0);
      if (cp < 0x20 || cp === 0x7f || /[/\\:*?"<>|]/.test(c)) {
        return "_";
      }
      return c;
    })
    .join("");
  if (sanitized.length === 0) {
    return "speaker";
  }
  return sanitized;
}

export function SpeakerAudioDownloads({
  meetingId,
  speakers,
  loading,
  error,
}: SpeakerAudioDownloadsProps) {
  if (loading) {
    return (
      <div className="speaker-audio-section">
        <h3>話者別音声（デバッグ）</h3>
        <div className="speaker-audio-loading">読み込み中...</div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="speaker-audio-section">
        <h3>話者別音声（デバッグ）</h3>
        <div className="speaker-audio-error">取得に失敗しました</div>
      </div>
    );
  }

  if (!speakers || speakers.length === 0) {
    return null;
  }

  const speakersWithAudio = speakers.filter((s) => s.has_audio);

  if (speakersWithAudio.length === 0) {
    return (
      <div className="speaker-audio-section">
        <h3>話者別音声（デバッグ）</h3>
        <div className="speaker-audio-empty">話者別音声はありません</div>
      </div>
    );
  }

  return (
    <div className="speaker-audio-section">
      <h3>話者別音声（デバッグ）</h3>
      <ul className="speaker-audio-list">
        {speakersWithAudio.map((speaker) => (
          <li key={speaker.speaker_id} className="speaker-audio-item">
            <span className="speaker-audio-name">{speaker.display_label}</span>
            <a
              href={getSpeakerAudioUrl(meetingId, speaker.speaker_id)}
              download={`${sanitizeFilename(speaker.display_label)}_speaker.wav`}
              className="speaker-audio-download"
              aria-label={`${speaker.display_label}の音声をダウンロード`}
            >
              ダウンロード
            </a>
          </li>
        ))}
      </ul>
    </div>
  );
}
