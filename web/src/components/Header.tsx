import { formatDate, formatDuration } from "../lib/formatters";
import type { MeetingResponse } from "../lib/types";

const STATUS_LABELS: Record<string, string> = {
  scheduled: "\u4e88\u5b9a",
  posted: "\u5b8c\u4e86",
  recording: "\u9332\u97f3\u4e2d",
  stopping: "\u505c\u6b62\u51e6\u7406\u4e2d",
  transcribing: "\u6587\u5b57\u8d77\u3053\u3057\u4e2d",
  summarizing: "\u8981\u7d04\u4e2d",
  processing: "\u51e6\u7406\u4e2d",
  failed: "\u5931\u6557",
  aborted: "\u4e2d\u6b62",
};

function statusClassName(status: string): string {
  return status.replace(/[^a-z0-9_-]/gi, "-").toLowerCase();
}

export function Header({ meeting }: { meeting: MeetingResponse | null }) {
  const title = meeting?.title || "--";
  const date = meeting?.started_at ? formatDate(meeting.started_at) : "--";
  const duration =
    meeting?.duration_seconds != null
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
            <span
              className={`status-badge status-${statusClassName(statusText)}`}
            >
              {statusLabel}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}
