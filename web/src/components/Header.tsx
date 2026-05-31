import { formatDate, formatDuration } from "../lib/formatters";
import { statusClassName, statusLabel } from "../lib/meetingStatus";
import type { MeetingResponse } from "../lib/types";

export function Header({ meeting }: { meeting: MeetingResponse | null }) {
  const title = meeting?.title || "--";
  const date = meeting?.started_at ? formatDate(meeting.started_at) : "--";
  const duration =
    meeting?.duration_seconds != null
      ? formatDuration(meeting.duration_seconds)
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
