import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { fetchGuildMeetings } from "../lib/api";
import { formatDate, formatDuration } from "../lib/formatters";
import {
  LIVE_MEETING_STATUSES,
  statusClassName,
  statusLabel,
} from "../lib/meetingStatus";
import type { MeetingListItem, MeetingListResponse } from "../lib/types";

const PAGE_SIZE = 20;

function displayTitle(meeting: MeetingListItem): string {
  return meeting.title || "\u7121\u984c\u306e\u4f1a\u8b70";
}

function displayDate(value: string | null): string {
  return value ? formatDate(value) : "--";
}

function displayDuration(value: number | null): string {
  return value != null ? formatDuration(value) : "--";
}

function meetingPath(meetingId: string): string {
  return `/meetings/${meetingId}`;
}

function dashboardErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message === "forbidden") {
    return "\u3053\u306e\u30ae\u30eb\u30c9\u306e\u4f1a\u8b70\u3092\u8868\u793a\u3059\u308b\u6a29\u9650\u304c\u3042\u308a\u307e\u305b\u3093";
  }
  return error instanceof Error
    ? error.message
    : "\u8aad\u307f\u8fbc\u307f\u306b\u5931\u6557\u3057\u307e\u3057\u305f";
}

interface DashboardPageProps {
  selectedGuildId?: string | null;
  selectedGuildName?: string;
  useCurrentGuildMeetings?: boolean;
  loadingGuildSelection?: boolean;
  noSelectableGuilds?: boolean;
}

export function DashboardPage({
  selectedGuildId,
  selectedGuildName,
  useCurrentGuildMeetings = false,
  loadingGuildSelection = false,
  noSelectableGuilds = false,
}: DashboardPageProps) {
  const [request, setRequest] = useState({ page: 1, reloadKey: 0 });
  const [data, setData] = useState<MeetingListResponse | null>(null);
  const [selectedVoiceChannelId, setSelectedVoiceChannelId] = useState<
    string | null
  >(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const previousGuildIdRef = useRef(selectedGuildId);
  const page = request.page;
  const responseGuildId = useCurrentGuildMeetings
    ? selectedGuildId
    : data?.guild_id;
  const activeData =
    data && selectedGuildId && responseGuildId === selectedGuildId
      ? data
      : null;

  useEffect(() => {
    document.title = "\u4f1a\u8b70\u4e00\u89a7";
  }, []);

  useEffect(() => {
    if (previousGuildIdRef.current === selectedGuildId) {
      return;
    }
    previousGuildIdRef.current = selectedGuildId;
    setSelectedVoiceChannelId(null);
    setRequest((current) => ({ ...current, page: 1 }));
  }, [selectedGuildId]);

  useEffect(() => {
    if (loadingGuildSelection) {
      setLoading(true);
      setError(null);
      return;
    }
    if (!selectedGuildId) {
      setData(null);
      setLoading(false);
      setError(null);
      return;
    }

    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setData(null);

    fetchGuildMeetings(
      useCurrentGuildMeetings ? null : selectedGuildId,
      request.page,
      PAGE_SIZE,
      selectedVoiceChannelId,
      controller.signal,
    )
      .then((response) => {
        if (
          !controller.signal.aborted &&
          (useCurrentGuildMeetings || response.guild_id === selectedGuildId)
        ) {
          setData(response);
        }
      })
      .catch((err: unknown) => {
        if (!controller.signal.aborted) {
          setError(dashboardErrorMessage(err));
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoading(false);
        }
      });

    return () => controller.abort();
  }, [
    request,
    selectedGuildId,
    selectedVoiceChannelId,
    useCurrentGuildMeetings,
    loadingGuildSelection,
  ]);

  const voiceChannelOptions = useMemo(() => {
    if (!activeData) {
      return [];
    }
    const options =
      activeData.voice_channels?.map((channel) => ({
        id: channel.id,
        label: channel.label || `VC ID: ${channel.id}`,
      })) ?? [];
    for (const meeting of activeData.meetings) {
      if (
        meeting.voice_channel_id &&
        !options.some((option) => option.id === meeting.voice_channel_id)
      ) {
        options.push({
          id: meeting.voice_channel_id,
          label: `VC ID: ${meeting.voice_channel_id}`,
        });
      }
    }
    if (
      selectedVoiceChannelId &&
      !options.some((option) => option.id === selectedVoiceChannelId)
    ) {
      options.push({
        id: selectedVoiceChannelId,
        label: `VC ID: ${selectedVoiceChannelId}`,
      });
    }
    return options;
  }, [activeData, selectedVoiceChannelId]);

  const totalPages = useMemo(() => {
    if (!activeData) {
      return 1;
    }
    return Math.max(1, Math.ceil(activeData.total / PAGE_SIZE));
  }, [activeData]);

  const showingFrom =
    activeData && activeData.total > 0 ? (request.page - 1) * PAGE_SIZE + 1 : 0;
  const showingTo =
    activeData && activeData.total > 0
      ? Math.min(request.page * PAGE_SIZE, activeData.total)
      : 0;
  const meetingCountText =
    activeData && activeData.total > 0
      ? `${showingFrom}-${showingTo} / ${activeData.total}`
      : selectedVoiceChannelId
        ? "\u3053\u306eVC\u306e\u4f1a\u8b70\u306f\u3042\u308a\u307e\u305b\u3093"
        : "\u4f1a\u8b70\u306f\u307e\u3060\u3042\u308a\u307e\u305b\u3093";
  const hasLiveMeetings =
    activeData?.meetings.some((meeting) =>
      LIVE_MEETING_STATUSES.has(meeting.status),
    ) ?? false;
  const emptyGuildMessage = noSelectableGuilds
    ? "\u8868\u793a\u3067\u304d\u308b\u30ae\u30eb\u30c9\u304c\u3042\u308a\u307e\u305b\u3093"
    : "\u30ae\u30eb\u30c9\u3092\u9078\u629e\u3057\u3066\u304f\u3060\u3055\u3044";
  const headerDescription = selectedGuildName
    ? `${selectedGuildName} / ${meetingCountText}`
    : meetingCountText;

  useEffect(() => {
    if (!hasLiveMeetings) {
      return;
    }
    const timer = window.setInterval(() => {
      setRequest((current) => ({
        ...current,
        reloadKey: current.reloadKey + 1,
      }));
    }, 15000);
    return () => window.clearInterval(timer);
  }, [hasLiveMeetings]);

  return (
    <main className="dashboard-page">
      <div className="dashboard-header">
        <div>
          <h1>{"\u4f1a\u8b70\u4e00\u89a7"}</h1>
          <p>
            {activeData
              ? headerDescription
              : loading
                ? "\u6700\u65b0\u306e\u4f1a\u8b70\u3092\u8aad\u307f\u8fbc\u3093\u3067\u3044\u307e\u3059"
                : selectedGuildId
                  ? meetingCountText
                  : emptyGuildMessage}
          </p>
        </div>
        <div className="dashboard-actions">
          <label className="dashboard-filter">
            <span>{"VC"}</span>
            <select
              aria-label="VC"
              value={selectedVoiceChannelId ?? ""}
              onChange={(event) => {
                const nextVoiceChannelId = event.target.value || null;
                setSelectedVoiceChannelId(nextVoiceChannelId);
                setRequest((current) => ({
                  page: 1,
                  reloadKey: current.reloadKey + 1,
                }));
              }}
              disabled={
                loading ||
                !selectedGuildId ||
                (!selectedVoiceChannelId && voiceChannelOptions.length === 0)
              }
            >
              <option value="">{"\u3059\u3079\u3066\u306eVC"}</option>
              {voiceChannelOptions.map((channel) => (
                <option key={channel.id} value={channel.id}>
                  {channel.label}
                </option>
              ))}
            </select>
          </label>
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
      </div>

      {error ? (
        <div className="panel-error dashboard-panel-message" role="alert">
          <span>{error}</span>
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
            {"\u518d\u8a66\u884c"}
          </button>
        </div>
      ) : !loading && !selectedGuildId ? (
        <div className="empty-state dashboard-panel-message">
          {emptyGuildMessage}
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
              {activeData?.meetings.map((meeting) => {
                const path = meetingPath(meeting.id);
                return (
                  <tr key={meeting.id} className="meeting-table-row">
                    <td>
                      <Link
                        className="meeting-title-link meeting-table-cell-link"
                        to={path}
                      >
                        {displayTitle(meeting)}
                      </Link>
                    </td>
                    <td>
                      <Link className="meeting-table-cell-link" to={path}>
                        <span
                          className={`status-badge status-${statusClassName(
                            meeting.status,
                          )}`}
                        >
                          {statusLabel(meeting.status)}
                        </span>
                      </Link>
                    </td>
                    <td>
                      <Link className="meeting-table-cell-link" to={path}>
                        {displayDate(meeting.started_at)}
                      </Link>
                    </td>
                    <td>
                      <Link className="meeting-table-cell-link" to={path}>
                        {displayDate(meeting.stopped_at)}
                      </Link>
                    </td>
                    <td className="meeting-duration">
                      <Link className="meeting-table-cell-link" to={path}>
                        {displayDuration(meeting.duration_seconds)}
                      </Link>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>

          {loading ? (
            <div className="loading dashboard-panel-message">
              <span className="loading-spinner" />
              {"\u8aad\u307f\u8fbc\u307f\u4e2d"}
            </div>
          ) : null}

          {!loading && activeData?.meetings.length === 0 ? (
            <div className="empty-state dashboard-panel-message">
              {selectedVoiceChannelId
                ? "\u3053\u306eVC\u306e\u4f1a\u8b70\u306f\u3042\u308a\u307e\u305b\u3093"
                : "\u4f1a\u8b70\u306f\u307e\u3060\u3042\u308a\u307e\u305b\u3093"}
            </div>
          ) : null}
        </section>
      )}

      {!error && activeData ? (
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
      ) : null}
    </main>
  );
}
