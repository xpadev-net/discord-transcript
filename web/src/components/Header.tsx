import { formatDate, formatDuration } from "../lib/formatters";
import { statusClassName, statusLabel } from "../lib/meetingStatus";
import type { MeetingResponse } from "../lib/types";

const MEETING_TITLE_DISPLAY_MAX_CHARS = 80;

export function Header({ meeting }: { meeting: MeetingResponse | null }) {
  const title = meeting ? displayMeetingTitle(meeting) : "--";
  const date = meeting?.started_at ? formatDate(meeting.started_at) : "--";
  const duration =
    meeting?.duration_seconds != null
      ? formatDuration(meeting.duration_seconds)
      : "--";
  const voiceChannel = meeting
    ? meeting.voice_channel_name?.trim() || `VC ID: ${meeting.voice_channel_id}`
    : "--";
  const statusText = meeting?.status || "unknown";
  const displayStatus = statusLabel(statusText);

  return (
    <div className="header">
      <div className="header-content">
        <h1>{title}</h1>
        <div className="header-meta">
          <div className="header-meta-item">
            <span className="label">{"\u65e5\u4ed8:"}</span>
            <span>{date}</span>
          </div>
          <div className="header-meta-item">
            <span className="label">{"\u6642\u9593:"}</span>
            <span>{duration}</span>
          </div>
          <div className="header-meta-item">
            <span className="label">{"VC:"}</span>
            <span>{voiceChannel}</span>
          </div>
          <div className="header-meta-item">
            <span
              className={`status-badge status-${statusClassName(statusText)}`}
            >
              {displayStatus}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}

function displayMeetingTitle(meeting: MeetingResponse): string {
  const title = meeting.title?.trim();
  if (
    title &&
    title.length <= MEETING_TITLE_DISPLAY_MAX_CHARS &&
    !hasControlCharacter(title)
  ) {
    return title;
  }
  const voiceChannel =
    meeting.voice_channel_name?.trim() || `VC ID: ${meeting.voice_channel_id}`;
  return meeting.started_at
    ? `${formatDate(meeting.started_at)} ${voiceChannel}`
    : voiceChannel;
}

function hasControlCharacter(value: string): boolean {
  return Array.from(value).some((character) => {
    const code = character.charCodeAt(0);
    return code < 32 || code === 127;
  });
}
