import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { fetchGuildMeetings, fetchGuildSettings } from "../lib/api";
import type { GuildSettingsResponse, MeetingListResponse } from "../lib/types";
import {
  ADMIN_MEETING_LIMIT,
  AdminError,
  AdminHeader,
  AdminLoading,
  AdminMetricCard,
  type AdminViewProps,
  adminErrorMessage,
  daysSince,
  displayDate,
  displayTitle,
  formatCount,
  isRetentionCandidate,
  meetingPath,
} from "./DashboardPage";

export function AdminRetentionPage({
  selectedGuildId,
  selectedGuildName,
}: AdminViewProps) {
  const [reloadKey, setReloadKey] = useState(0);
  const [settings, setSettings] = useState<GuildSettingsResponse | null>(null);
  const [meetings, setMeetings] = useState<MeetingListResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    document.title = "Retention";
  }, []);

  useEffect(() => {
    void reloadKey;
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    Promise.all([
      fetchGuildSettings(undefined, controller.signal),
      fetchGuildMeetings(null, 1, ADMIN_MEETING_LIMIT, null, controller.signal),
    ])
      .then(([settingsResponse, meetingsResponse]) => {
        if (!controller.signal.aborted) {
          setSettings(settingsResponse);
          setMeetings(meetingsResponse);
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
  }, [reloadKey]);

  const meetingItems = meetings?.meetings ?? [];
  const rawTtl = settings?.retention_raw_audio_ttl_days ?? 0;
  const transcriptTtl = settings?.retention_transcript_ttl_days ?? 0;
  const candidates = meetingItems.filter((meeting) => {
    const candidateDate = meeting.stopped_at ?? meeting.started_at;
    return (
      isRetentionCandidate(candidateDate, rawTtl) ||
      isRetentionCandidate(candidateDate, transcriptTtl)
    );
  });

  return (
    <main className="admin-page">
      <AdminHeader
        title="Retention"
        description="削除候補と保持ポリシー"
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
          <section className="admin-metric-grid admin-wide-section">
            <AdminMetricCard
              label="Raw audio TTL"
              value={`${rawTtl || "--"} days`}
              detail="Settings value"
            />
            <AdminMetricCard
              label="Transcript TTL"
              value={`${transcriptTtl || "--"} days`}
              detail="Settings value"
            />
            <AdminMetricCard
              label="Deletion candidates"
              value={formatCount(candidates.length)}
              detail={`visible ${meetingItems.length} meetings`}
            />
          </section>

          <section className="admin-table-shell admin-wide-section">
            <table className="meeting-table admin-table">
              <thead>
                <tr>
                  <th scope="col">{"Meeting"}</th>
                  <th scope="col">{"Stopped"}</th>
                  <th scope="col">{"Age"}</th>
                  <th scope="col">{"Raw audio"}</th>
                  <th scope="col">{"Transcript"}</th>
                </tr>
              </thead>
              <tbody>
                {candidates.map((meeting) => {
                  const candidateDate =
                    meeting.stopped_at ?? meeting.started_at;
                  const ageDays = daysSince(candidateDate);
                  const rawCandidate = isRetentionCandidate(
                    candidateDate,
                    rawTtl,
                  );
                  const transcriptCandidate = isRetentionCandidate(
                    candidateDate,
                    transcriptTtl,
                  );
                  return (
                    <tr key={meeting.id}>
                      <td>
                        <Link
                          className="meeting-title-link meeting-table-cell-link"
                          to={meetingPath(meeting.id, selectedGuildId)}
                        >
                          {displayTitle(meeting)}
                        </Link>
                      </td>
                      <td>{displayDate(candidateDate)}</td>
                      <td>{ageDays === null ? "--" : `${ageDays} days`}</td>
                      <td>{rawCandidate ? "Candidate" : "--"}</td>
                      <td>{transcriptCandidate ? "Candidate" : "--"}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {candidates.length === 0 ? (
              <div className="empty-state dashboard-panel-message">
                {"表示中の会議には削除候補がありません"}
              </div>
            ) : null}
          </section>
        </div>
      )}
    </main>
  );
}
