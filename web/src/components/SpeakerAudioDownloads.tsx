import type { SpeakerAudioInfo } from "../lib/types";
import { getSpeakerAudioUrl } from "../lib/api";

interface SpeakerAudioDownloadsProps {
  meetingId: string;
  speakers: SpeakerAudioInfo[] | null;
  loading: boolean;
}

export function SpeakerAudioDownloads({
  meetingId,
  speakers,
  loading,
}: SpeakerAudioDownloadsProps) {
  if (loading) {
    return (
      <div className="speaker-audio-section">
        <h3>{"\u8a71\u8005\u5225\u97f3\u58f0"}</h3>
        <div className="speaker-audio-loading">{"\u8aad\u307f\u8fbc\u307f\u4e2d..."}</div>
      </div>
    );
  }

  if (!speakers || speakers.length === 0) {
    return null;
  }

  const speakersWithAudio = speakers.filter((s) => s.has_audio);

  if (speakersWithAudio.length === 0) {
    return null;
  }

  return (
    <div className="speaker-audio-section">
      <h3>{"\u8a71\u8005\u5225\u97f3\u58f0\uff08\u30c7\u30d0\u30c3\u30b0\uff09"}</h3>
      <ul className="speaker-audio-list">
        {speakersWithAudio.map((speaker) => (
          <li key={speaker.speaker_id} className="speaker-audio-item">
            <span className="speaker-audio-name">{speaker.display_label}</span>
            <a
              href={getSpeakerAudioUrl(meetingId, speaker.speaker_id)}
              download={`${speaker.display_label}_speaker.wav`}
              className="speaker-audio-download"
            >
              {"\u30c0\u30a6\u30f3\u30ed\u30fc\u30c9"}
            </a>
          </li>
        ))}
      </ul>
    </div>
  );
}
