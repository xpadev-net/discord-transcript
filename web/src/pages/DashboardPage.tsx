import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { fetchGuildMeetings } from "../lib/api";
import { formatDate, formatDuration } from "../lib/formatters";
import type { MeetingListItem, MeetingListResponse } from "../lib/types";

const PAGE_SIZE = 20;

const STATUS_LABELS: Record<string, string> = {
  scheduled: "\u4e88\u5b9a",
  recording: "\u9332\u97f3\u4e2d",
  stopping: "\u505c\u6b62\u51e6\u7406\u4e2d",
  transcribing: "\u6587\u5b57\u8d77\u3053\u3057\u4e2d",
  summarizing: "\u8981\u7d04\u4e2d",
  posted: "\u5b8c\u4e86",
  failed: "\u5931\u6557",
  aborted: "\u4e2d\u6b62",
  processing: "\u51e6\u7406\u4e2d",
};

function statusLabel(status: string): string {
  return STATUS_LABELS[status] ?? status;
}

function statusClassName(status: string): string {
  return status.replace(/[^a-z0-9_-]/gi, "-").toLowerCase();
}

function displayTitle(meeting: MeetingListItem): string {
  return meeting.title || "\u7121\u984c\u306e\u4f1a\u8b70";
}

function displayDate(value: string | null): string {
  return value ? formatDate(value) : "--";
}

function displayDuration(value: number | null): string {
  return value != null ? formatDuration(value) : "--";
}

export function DashboardPage() {
  const navigate = useNavigate();
  const [request, setRequest] = useState({ page: 1, reloadKey: 0 });
  const [data, setData] = useState<MeetingListResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const page = request.page;

  useEffect(() => {
    document.title = "\u4f1a\u8b70\u4e00\u89a7";
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);

    fetchGuildMeetings(request.page, PAGE_SIZE, controller.signal)
      .then((response) => {
        if (!controller.signal.aborted) {
          setData(response);
        }
      })
      .catch((err: unknown) => {
        if (!controller.signal.aborted) {
          setError(
            err instanceof Error
              ? err.message
              : "\u8aad\u307f\u8fbc\u307f\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
          );
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoading(false);
        }
      });

    return () => controller.abort();
  }, [request]);

  const totalPages = useMemo(() => {
    if (!data) {
      return 1;
    }
    return Math.max(1, Math.ceil(data.total / data.limit));
  }, [data]);

  const showingFrom =
    data && data.total > 0 ? (data.page - 1) * data.limit + 1 : 0;
  const showingTo = data ? Math.min(data.page * data.limit, data.total) : 0;

  return (
    <main className="dashboard-page">
      <div className="dashboard-header">
        <div>
          <h1>{"\u4f1a\u8b70\u4e00\u89a7"}</h1>
          <p>
            {data
              ? `${showingFrom}-${showingTo} / ${data.total}`
              : "\u6700\u65b0\u306e\u4f1a\u8b70\u3092\u8aad\u307f\u8fbc\u3093\u3067\u3044\u307e\u3059"}
          </p>
        </div>
        <button
          type="button"
          className="secondary-button"
          onClick={() =>
            setRequest((current) => ({
              ...current,
              reloadKey: current.reloadKey + 1,
            }))
          }
          disabled={loading}
        >
          {"\u518d\u8aad\u307f\u8fbc\u307f"}
        </button>
      </div>

      {error ? (
        <div className="panel-error dashboard-panel-message" role="alert">
          <span>{error}</span>
          <button
            type="button"
            onClick={() =>
              setRequest((current) => ({
                ...current,
                reloadKey: current.reloadKey + 1,
              }))
            }
          >
            {"\u518d\u8a66\u884c"}
          </button>
        </div>
      ) : (
        <section className="dashboard-table-shell" aria-busy={loading}>
          <table className="meeting-table">
            <thead>
              <tr>
                <th scope="col">{"\u30bf\u30a4\u30c8\u30eb"}</th>
                <th scope="col">{"\u30b9\u30c6\u30fc\u30bf\u30b9"}</th>
                <th scope="col">{"\u958b\u59cb"}</th>
                <th scope="col">{"\u7d42\u4e86"}</th>
                <th scope="col">{"\u6642\u9593"}</th>
              </tr>
            </thead>
            <tbody>
              {data?.meetings.map((meeting) => (
                <tr
                  key={meeting.id}
                  className="meeting-table-row"
                  onClick={() => navigate(`/meetings/${meeting.id}`)}
                >
                  <td>
                    <Link
                      className="meeting-title-link"
                      to={`/meetings/${meeting.id}`}
                      onClick={(event) => event.stopPropagation()}
                    >
                      {displayTitle(meeting)}
                    </Link>
                  </td>
                  <td>
                    <span
                      className={`status-badge status-${statusClassName(
                        meeting.status,
                      )}`}
                    >
                      {statusLabel(meeting.status)}
                    </span>
                  </td>
                  <td>{displayDate(meeting.started_at)}</td>
                  <td>{displayDate(meeting.stopped_at)}</td>
                  <td className="meeting-duration">
                    {displayDuration(meeting.duration_seconds)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          {loading ? (
            <div className="loading dashboard-panel-message">
              <span className="loading-spinner" />
              {"\u8aad\u307f\u8fbc\u307f\u4e2d"}
            </div>
          ) : null}

          {!loading && data?.meetings.length === 0 ? (
            <div className="empty-state dashboard-panel-message">
              {"\u4f1a\u8b70\u306f\u307e\u3060\u3042\u308a\u307e\u305b\u3093"}
            </div>
          ) : null}
        </section>
      )}

      <div className="pagination">
        <button
          type="button"
          className="secondary-button"
          onClick={() =>
            setRequest((current) => ({
              ...current,
              page: Math.max(1, current.page - 1),
            }))
          }
          disabled={loading || page <= 1}
        >
          {"\u524d\u3078"}
        </button>
        <span>
          {page} / {totalPages}
        </span>
        <button
          type="button"
          className="secondary-button"
          onClick={() =>
            setRequest((current) => ({
              ...current,
              page: Math.min(totalPages, current.page + 1),
            }))
          }
          disabled={loading || page >= totalPages}
        >
          {"\u6b21\u3078"}
        </button>
      </div>
    </main>
  );
}
