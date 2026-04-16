import { formatTimestamp } from "../lib/formatters";
import { getSpeakerColor } from "../lib/speakers";
import type { TranscriptSegment as Segment } from "../lib/types";

interface Props {
  segment: Segment;
  isActive: boolean;
  onSeek: (startMs: number) => void;
}

function normalizeSpeaker(seg: Segment) {
  const speaker = seg.speaker;
  const id = speaker?.id || seg.speaker_id || "";
  const nickname = speaker?.nickname || null;
  const username = speaker?.username || null;
  const displayName = speaker?.display_name || null;
  const displayLabel =
    speaker?.display_label || nickname || displayName || username || id;

  return { id, username, nickname, displayLabel };
}

function SpeakerMeta({
  speaker,
}: {
  speaker: ReturnType<typeof normalizeSpeaker>;
}) {
  const parts: string[] = [];
  if (speaker.nickname) parts.push(`Nick: ${speaker.nickname}`);
  if (speaker.username) parts.push(`User: ${speaker.username}`);
  if (speaker.id) parts.push(`ID: ${speaker.id}`);
  if (parts.length === 0) return null;
  return <span className="speaker-meta">{parts.join(" / ")}</span>;
}

export function TranscriptSegmentRow({ segment, isActive, onSeek }: Props) {
  const speaker = normalizeSpeaker(segment);
  const color = getSpeakerColor(speaker.id || segment.speaker_id);
  const isVcText = segment.source === "vc_text";

  const handleClick = () => onSeek(segment.start_ms);
  const className = [
    "segment",
    isActive && "active",
    segment.is_noisy && "noisy",
    isVcText && "vc-text",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <button type="button" className={className} onClick={handleClick}>
      <span className="segment-meta">
        <span className="speaker-badge" style={{ background: color }}>
          {speaker.displayLabel}
        </span>
        <SpeakerMeta speaker={speaker} />
        <span className="segment-time">
          {formatTimestamp(segment.start_ms)}
        </span>
        {isVcText && <span className="segment-source">Chat</span>}
      </span>
      <span className="segment-text">{segment.text}</span>
    </button>
  );
}
