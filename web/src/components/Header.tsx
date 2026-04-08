import type { MeetingResponse } from "../lib/types";
import { formatDate, formatDuration } from "../lib/formatters";

const STATUS_LABELS: Record<string, string> = {
  posted: "\u5b8c\u4e86",
  recording: "\u9332\u97f3\u4e2d",
  processing: "\u51e6\u7406\u4e2d",
};

export function Header({ meeting }: { meeting: MeetingResponse | null }) {
  const title = meeting?.title || "--";
  const date = meeting?.started_at ? formatDate(meeting.started_at) : "--";
  const duration = meeting?.duration_seconds
    ? formatDuration(meeting.duration_seconds)
    : "--";
  const statusText = meeting?.status || "unknown";
  const statusLabel = STATUS_LABELS[statusText] || statusText;

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
            <span className={`status-badge status-${statusText}`}>
              {statusLabel}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}
