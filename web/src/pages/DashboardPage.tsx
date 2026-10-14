import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import {
  fetchGuildJobs,
  fetchGuildMeetings,
  fetchGuildSettings,
  isApiError,
  retryGuildJob,
} from "../lib/api";
import { formatDate, formatDuration } from "../lib/formatters";
import {
  LIVE_MEETING_STATUSES,
  statusClassName,
  statusLabel,
} from "../lib/meetingStatus";
import { displayMeetingTitle } from "../lib/meetingTitle";
import type {
  GuildJob,
  GuildJobStatus,
  GuildJobType,
  GuildSettingsResponse,
  MeetingListItem,
  MeetingListResponse,
} from "../lib/types";

const PAGE_SIZE = 20;

export function displayTitle(meeting: MeetingListItem): string {
  return displayMeetingTitle(meeting);
}

export function displayDate(value: string | null): string {
  return value ? formatDate(value) : "--";
}

function displayDuration(value: number | null): string {
  return value != null ? formatDuration(value) : "--";
}

function displayVoiceChannel(meeting: MeetingListItem): string {
  return (
    meeting.voice_channel_name?.trim() || `VC ID: ${meeting.voice_channel_id}`
  );
}

export function meetingPath(
  meetingId: string,
  guildId?: string | null,
): string {
  const path = `/meetings/${encodeURIComponent(meetingId)}`;
  return guildId ? `${path}?guild_id=${encodeURIComponent(guildId)}` : path;
}

function dashboardErrorMessage(error: unknown): string {
  if (isApiError(error) && error.status === 403) {
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
  const [request, setRequest] = useState({
    page: 1,
    reloadKey: 0,
    cursor: null as string | null,
    cursorHistory: [] as Array<string | null>,
  });
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
    setRequest((current) => ({
      ...current,
      page: 1,
      cursor: null,
      cursorHistory: [],
    }));
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
      { mode: "cursor", cursor: request.cursor },
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
          label: displayVoiceChannel(meeting),
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
    if (!activeData || activeData.total === null) {
      return request.page;
    }
    return Math.max(1, Math.ceil(activeData.total / PAGE_SIZE));
  }, [activeData, request.page]);

  const showingFrom =
    activeData && activeData.meetings.length > 0
      ? (request.page - 1) * PAGE_SIZE + 1
      : 0;
  const showingTo =
    activeData && activeData.meetings.length > 0
      ? showingFrom + activeData.meetings.length - 1
      : 0;
  const meetingCountText =
    activeData && activeData.meetings.length > 0
      ? activeData.total === null
        ? `${showingFrom}-${showingTo}`
        : `${showingFrom}-${showingTo} / ${activeData.total}`
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
                  cursor: null,
                  cursorHistory: [],
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
                <th scope="col">{"VC"}</th>
                <th scope="col">{"\u30b9\u30c6\u30fc\u30bf\u30b9"}</th>
                <th scope="col">{"\u958b\u59cb"}</th>
                <th scope="col">{"\u7d42\u4e86"}</th>
                <th scope="col">{"\u6642\u9593"}</th>
              </tr>
            </thead>
            <tbody>
              {activeData?.meetings.map((meeting) => {
                const path = meetingPath(meeting.id, activeData?.guild_id);
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
                        {displayVoiceChannel(meeting)}
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
                cursor: current.cursorHistory.at(-1) ?? null,
                cursorHistory: current.cursorHistory.slice(0, -1),
              }))
            }
            disabled={loading || request.cursorHistory.length === 0}
          >
            {"\u524d\u3078"}
          </button>
          <span>
            {activeData.total === null
              ? `${page}ページ`
              : `${page} / ${totalPages}`}
          </span>
          <button
            type="button"
            className="secondary-button"
            onClick={() =>
              setRequest((current) => ({
                ...current,
                page: current.page + 1,
                cursor: activeData.next_cursor,
                cursorHistory: [...current.cursorHistory, current.cursor],
              }))
            }
            disabled={loading || !activeData.next_cursor}
          >
            {"\u6b21\u3078"}
          </button>
        </div>
      ) : null}
    </main>
  );
}

export const ADMIN_MEETING_LIMIT = 100;
const ADMIN_JOB_LIMIT = 100;

const jobStatuses: Array<GuildJobStatus | ""> = [
  "",
  "failed",
  "queued",
  "running",
  "done",
  "canceled",
];
const jobTypes: Array<GuildJobType | ""> = [
  "",
  "transcribe",
  "summarize",
  "cleanup",
];

const jobStatusLabels: Record<GuildJobStatus, string> = {
  queued: "Queued",
  running: "Running",
  failed: "Failed",
  done: "Done",
  canceled: "Canceled",
};

const jobTypeLabels: Record<GuildJobType, string> = {
  transcribe: "Transcribe",
  summarize: "Summarize",
  cleanup: "Cleanup",
};

function jobStatusLabel(status: string): string {
  return jobStatusLabels[status as GuildJobStatus] ?? status;
}

function jobTypeLabel(jobType: string): string {
  return jobTypeLabels[jobType as GuildJobType] ?? jobType;
}

export function adminErrorMessage(error: unknown): string {
  if (isApiError(error) && error.status === 403) {
    return "この管理ビューを表示する権限がありません";
  }
  return error instanceof Error
    ? error.message
    : "管理情報の読み込みに失敗しました";
}

export function formatCount(value: number): string {
  return new Intl.NumberFormat().format(value);
}

function formatPercent(numerator: number, denominator: number): string {
  if (denominator <= 0) {
    return "--";
  }
  return `${Math.round((numerator / denominator) * 100)}%`;
}

function formatStorageUnavailable(): string {
  return "API未提供";
}

function latestIso(values: Array<string | null>): string | null {
  const newest = values
    .filter((value): value is string => Boolean(value))
    .map((value) => new Date(value).getTime())
    .filter((value) => !Number.isNaN(value))
    .sort((a, b) => b - a)[0];
  return newest === undefined ? null : new Date(newest).toISOString();
}

export function daysSince(
  value: string | null,
  now = Date.now(),
): number | null {
  if (!value) {
    return null;
  }
  const time = new Date(value).getTime();
  if (Number.isNaN(time)) {
    return null;
  }
  return Math.max(0, Math.floor((now - time) / 86_400_000));
}

export function isRetentionCandidate(
  value: string | null,
  ttlDays: number,
): boolean {
  const ageDays = daysSince(value);
  return ageDays !== null && ageDays >= ttlDays;
}

function isRetryableJob(job: GuildJob): boolean {
  return job.status === "failed" && job.dead_lettered_at === null;
}

export function AdminHeader({
  title,
  description,
  guildName,
  actions,
}: {
  title: string;
  description: string;
  guildName?: string;
  actions?: ReactNode;
}) {
  return (
    <div className="admin-view-header">
      <div>
        <h1>{title}</h1>
        <p>{guildName ? `${guildName} / ${description}` : description}</p>
      </div>
      {actions ? <div className="admin-view-actions">{actions}</div> : null}
    </div>
  );
}

export function AdminLoading() {
  return (
    <output className="loading settings-panel-message">
      <span className="loading-spinner" />
      {"読み込み中"}
    </output>
  );
}

export function AdminError({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="panel-error settings-panel-message" role="alert">
      <span>{message}</span>
      <button type="button" onClick={onRetry}>
        {"再試行"}
      </button>
    </div>
  );
}

export function AdminMetricCard({
  label,
  value,
  detail,
}: {
  label: string;
  value: string;
  detail: string;
}) {
  return (
    <div className="admin-metric-card">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{detail}</small>
    </div>
  );
}

export interface AdminViewProps {
  selectedGuildId?: string | null;
  selectedGuildName?: string;
  isSystemAdmin?: boolean;
  canManageSettings?: boolean;
  canReprocessMeetings?: boolean;
}

export function AdminUsagePage({
  selectedGuildName,
  isSystemAdmin = false,
  canManageSettings = false,
}: AdminViewProps) {
  const [reloadKey, setReloadKey] = useState(0);
  const [settings, setSettings] = useState<GuildSettingsResponse | null>(null);
  const [meetings, setMeetings] = useState<MeetingListResponse | null>(null);
  const [jobs, setJobs] = useState<GuildJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    document.title = "Usage";
  }, []);

  useEffect(() => {
    void reloadKey;
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    Promise.all([
      canManageSettings
        ? fetchGuildSettings(undefined, controller.signal)
        : Promise.resolve(null),
      fetchGuildMeetings(null, 1, ADMIN_MEETING_LIMIT, null, controller.signal),
      fetchGuildJobs({ limit: ADMIN_JOB_LIMIT, signal: controller.signal }),
    ])
      .then(([settingsResponse, meetingsResponse, jobsResponse]) => {
        if (!controller.signal.aborted) {
          setSettings(settingsResponse);
          setMeetings(meetingsResponse);
          setJobs(jobsResponse);
        }
      })
      .catch((err: unknown) => {
        if (!controller.signal.aborted) {
          setError(adminErrorMessage(err));
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoading(false);
        }
      });
    return () => controller.abort();
  }, [canManageSettings, reloadKey]);

  const meetingItems = meetings?.meetings ?? [];
  const totalDurationSeconds = meetingItems.reduce(
    (sum, meeting) => sum + (meeting.duration_seconds ?? 0),
    0,
  );
  const completedMeetings = meetingItems.filter((meeting) =>
    ["completed", "posted"].includes(meeting.status),
  ).length;
  const failedJobs = jobs.filter((job) => job.status === "failed").length;
  const retryableJobs = jobs.filter(isRetryableJob).length;
  const newestActivity = latestIso([
    ...meetingItems.map((meeting) => meeting.stopped_at ?? meeting.started_at),
    ...jobs.map((job) => job.updated_at),
  ]);

  return (
    <main className="admin-page">
      <AdminHeader
        title="Usage"
        description="利用状況と保持ポリシー"
        guildName={selectedGuildName}
        actions={
          <button
            type="button"
            className="secondary-button"
            onClick={() => setReloadKey((current) => current + 1)}
            disabled={loading}
          >
            {"再読み込み"}
          </button>
        }
      />

      {error ? (
        <AdminError
          message={error}
          onRetry={() => setReloadKey((current) => current + 1)}
        />
      ) : loading ? (
        <AdminLoading />
      ) : (
        <div className="admin-view-layout">
          <section className="settings-section admin-wide-section">
            <div className="settings-section-heading">
              <div>
                <h2>{"Plan / Entitlement"}</h2>
                <p>
                  {
                    "読み取り専用のギルド別プランAPIは未提供です。システム管理者は既存のプラン編集画面で割り当てとクォータを確認できます。"
                  }
                </p>
              </div>
              {isSystemAdmin ? (
                <Link className="secondary-button" to="/admin/plans">
                  {"Plans"}
                </Link>
              ) : null}
            </div>
          </section>

          <section className="admin-metric-grid admin-wide-section">
            <AdminMetricCard
              label="Meetings"
              value={formatCount(meetings?.total ?? meetingItems.length)}
              detail={`最新${meetingItems.length}件を集計`}
            />
            <AdminMetricCard
              label="Recording minutes"
              value={formatCount(Math.round(totalDurationSeconds / 60))}
              detail={`${completedMeetings} completed / visible sample`}
            />
            <AdminMetricCard
              label="Failed jobs"
              value={formatCount(failedJobs)}
              detail={`${retryableJobs} retryable`}
            />
            <AdminMetricCard
              label="Storage"
              value={formatStorageUnavailable()}
              detail="storage_bytes の利用量API待ち"
            />
          </section>

          <section className="settings-section">
            <h2>{"Usage breakdown"}</h2>
            <dl className="admin-definition-list">
              <div>
                <dt>{"Summary enabled"}</dt>
                <dd>
                  {settings
                    ? settings.summary_enabled
                      ? "Enabled"
                      : "Disabled"
                    : "--"}
                </dd>
              </div>
              <div>
                <dt>{"Meeting completion rate"}</dt>
                <dd>{formatPercent(completedMeetings, meetingItems.length)}</dd>
              </div>
              <div>
                <dt>{"Raw audio TTL"}</dt>
                <dd>{settings?.retention_raw_audio_ttl_days ?? "--"} days</dd>
              </div>
              <div>
                <dt>{"Transcript TTL"}</dt>
                <dd>{settings?.retention_transcript_ttl_days ?? "--"} days</dd>
              </div>
            </dl>
          </section>

          <section className="settings-section">
            <h2>{"Recent activity"}</h2>
            <dl className="admin-definition-list">
              <div>
                <dt>{"Last visible activity"}</dt>
                <dd>{newestActivity ? formatDate(newestActivity) : "--"}</dd>
              </div>
              <div>
                <dt>{"Queued / running jobs"}</dt>
                <dd>
                  {
                    jobs.filter((job) =>
                      ["queued", "running"].includes(job.status),
                    ).length
                  }
                </dd>
              </div>
            </dl>
          </section>
        </div>
      )}
    </main>
  );
}

export function AdminJobsPage({
  selectedGuildName,
  canReprocessMeetings = false,
}: AdminViewProps) {
  const [filters, setFilters] = useState<{
    status: GuildJobStatus | "";
    jobType: GuildJobType | "";
  }>({ status: "failed", jobType: "" });
  const [reloadKey, setReloadKey] = useState(0);
  const [jobs, setJobs] = useState<GuildJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [retryingJobId, setRetryingJobId] = useState<string | null>(null);
  const [retryError, setRetryError] = useState<string | null>(null);

  useEffect(() => {
    document.title = "Jobs";
  }, []);

  useEffect(() => {
    void reloadKey;
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setRetryError(null);
    fetchGuildJobs({
      status: filters.status,
      jobType: filters.jobType,
      limit: ADMIN_JOB_LIMIT,
      signal: controller.signal,
    })
      .then((response) => {
        if (!controller.signal.aborted) {
          setJobs(response);
        }
      })
      .catch((err: unknown) => {
        if (!controller.signal.aborted) {
          setError(adminErrorMessage(err));
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoading(false);
        }
      });
    return () => controller.abort();
  }, [filters, reloadKey]);

  const failedJobs = jobs.filter((job) => job.status === "failed").length;
  const retryableJobs = jobs.filter(isRetryableJob).length;

  async function handleRetry(job: GuildJob) {
    if (
      !canReprocessMeetings ||
      job.job_type !== "summarize" ||
      !["failed", "canceled"].includes(job.status)
    ) {
      return;
    }
    if (!window.confirm("この会議の要約を再実行しますか？")) {
      return;
    }
    setRetryError(null);
    setRetryingJobId(job.id);
    try {
      const retriedJob = await retryGuildJob(job.id);
      setJobs((current) =>
        current.map((currentJob) =>
          currentJob.id === retriedJob.id ? retriedJob : currentJob,
        ),
      );
      setReloadKey((current) => current + 1);
    } catch (retryFailure) {
      setRetryError(
        isApiError(retryFailure) &&
          retryFailure.code === "retry_input_unavailable"
          ? "元音声が保持されていないため、要約を再実行できません。"
          : "要約の再実行に失敗しました。ジョブを再読み込みして確認してください。",
      );
    } finally {
      setRetryingJobId(null);
    }
  }

  return (
    <main className="admin-page">
      <AdminHeader
        title="Jobs"
        description="失敗・再実行待ちジョブ"
        guildName={selectedGuildName}
        actions={
          <button
            type="button"
            className="secondary-button"
            onClick={() => setReloadKey((current) => current + 1)}
            disabled={loading}
          >
            {"再読み込み"}
          </button>
        }
      />
      <section className="settings-section">
        <div className="admin-filter-row">
          <label className="settings-field">
            <span>{"Status"}</span>
            <select
              value={filters.status}
              onChange={(event) =>
                setFilters((current) => ({
                  ...current,
                  status: event.target.value as GuildJobStatus | "",
                }))
              }
            >
              {jobStatuses.map((status) => (
                <option key={status || "all"} value={status}>
                  {status ? jobStatusLabel(status) : "All statuses"}
                </option>
              ))}
            </select>
          </label>
          <label className="settings-field">
            <span>{"Type"}</span>
            <select
              value={filters.jobType}
              onChange={(event) =>
                setFilters((current) => ({
                  ...current,
                  jobType: event.target.value as GuildJobType | "",
                }))
              }
            >
              {jobTypes.map((jobType) => (
                <option key={jobType || "all"} value={jobType}>
                  {jobType ? jobTypeLabel(jobType) : "All types"}
                </option>
              ))}
            </select>
          </label>
        </div>
      </section>

      {error ? (
        <AdminError
          message={error}
          onRetry={() => setReloadKey((current) => current + 1)}
        />
      ) : loading ? (
        <AdminLoading />
      ) : (
        <>
          {retryError ? (
            <p className="panel-error settings-panel-message" role="alert">
              {retryError}
            </p>
          ) : null}
          <section className="admin-metric-grid">
            <AdminMetricCard
              label="Visible jobs"
              value={formatCount(jobs.length)}
              detail={`limit ${ADMIN_JOB_LIMIT}`}
            />
            <AdminMetricCard
              label="Failed"
              value={formatCount(failedJobs)}
              detail={`${retryableJobs} retryable`}
            />
            <AdminMetricCard
              label="Running"
              value={formatCount(
                jobs.filter((job) => job.status === "running").length,
              )}
              detail="lease付きジョブ"
            />
          </section>

          <section className="admin-table-shell">
            <table className="meeting-table admin-table">
              <thead>
                <tr>
                  <th scope="col">{"Job"}</th>
                  <th scope="col">{"Status"}</th>
                  <th scope="col">{"Retry"}</th>
                  <th scope="col">{"Next run"}</th>
                  <th scope="col">{"Updated"}</th>
                  <th scope="col">{"Error"}</th>
                  {canReprocessMeetings ? (
                    <th scope="col">{"Actions"}</th>
                  ) : null}
                </tr>
              </thead>
              <tbody>
                {jobs.map((job) => (
                  <tr key={job.id}>
                    <td>
                      <div className="admin-table-primary">
                        <strong>{jobTypeLabel(job.job_type)}</strong>
                        <span>{job.meeting_id}</span>
                      </div>
                    </td>
                    <td>
                      <span
                        className={`status-badge status-${statusClassName(
                          job.status,
                        )}`}
                      >
                        {jobStatusLabel(job.status)}
                      </span>
                    </td>
                    <td>{job.retry_count}</td>
                    <td>
                      {job.next_run_at ? formatDate(job.next_run_at) : "--"}
                    </td>
                    <td>{formatDate(job.updated_at)}</td>
                    <td>{job.error_message ?? job.cancel_reason ?? "--"}</td>
                    {canReprocessMeetings ? (
                      <td>
                        {job.job_type === "summarize" &&
                        ["failed", "canceled"].includes(job.status) ? (
                          <button
                            type="button"
                            className="secondary-button"
                            onClick={() => void handleRetry(job)}
                            disabled={retryingJobId !== null}
                          >
                            {retryingJobId === job.id
                              ? "再実行中…"
                              : "要約を再実行"}
                          </button>
                        ) : null}
                      </td>
                    ) : null}
                  </tr>
                ))}
              </tbody>
            </table>
            {jobs.length === 0 ? (
              <div className="empty-state dashboard-panel-message">
                {"条件に一致するジョブはありません"}
              </div>
            ) : null}
          </section>
        </>
      )}
    </main>
  );
}

export function AdminAuditPage({
  selectedGuildName,
  isSystemAdmin = false,
}: AdminViewProps) {
  useEffect(() => {
    document.title = "Audit";
  }, []);

  return (
    <main className="admin-page">
      <AdminHeader
        title="Audit"
        description="監査イベント"
        guildName={selectedGuildName}
      />
      <section className="settings-section">
        <div className="settings-section-heading">
          <div>
            <h2>{"Recent audit events"}</h2>
            <p>
              {
                "監査ログはバックエンドで記録されていますが、このフロントエンドから利用できる読み取りAPIはまだありません。"
              }
            </p>
          </div>
          {isSystemAdmin ? (
            <Link className="secondary-button" to="/admin/plans">
              {"Plan audit sources"}
            </Link>
          ) : null}
        </div>
        <dl className="admin-definition-list">
          <div>
            <dt>{"Supported writes"}</dt>
            <dd>
              {"settings.update, job.retry, job.cancel, plan/quota changes"}
            </dd>
          </div>
          <div>
            <dt>{"Readable events"}</dt>
            <dd>{"API未提供"}</dd>
          </div>
        </dl>
      </section>
    </main>
  );
}
