import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { fetchGuildMeetings } from "../lib/api";
import type { MeetingListItem } from "../lib/types";
import { formatDate, formatDuration } from "../lib/formatters";

const statusLabels: Record<string, string> = {
  posted: "完了",
  recording: "録音中",
  summarizing: "要約生成中",
  transcribing: "文字起こし中",
  stopping: "停止中",
  failed: "失敗",
  queued: "キュー待ち",
};

function getStatusLabel(status: string): string {
  return statusLabels[status] || status;
}

export function DashboardPage() {
  const [meetings, setMeetings] = useState<MeetingListItem[]>([]);
  const [page, setPage] = useState(1);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    
    fetchGuildMeetings(page, 20, controller.signal)
      .then((res) => {
        setMeetings(res.meetings);
        setTotal(res.total);
        setPage(res.page);
      })
      .catch((err) => {
        if (err.name !== "AbortError") {
          setError(err.message);
        }
      })
      .finally(() => setLoading(false));

    return () => controller.abort();
  }, [page]);

  const totalPages = Math.ceil(total / 20);

  if (error) {
    return <div className="dashboard-error">{error}</div>;
  }

  return (
    <div className="dashboard">
      <h1>会議一覧</h1>
      
      {loading ? (
        <div className="loading-spinner">読み込み中...</div>
      ) : meetings.length === 0 ? (
        <div className="empty-state">会議が見つかりませんでした</div>
      ) : (
        <>
          <table className="meetings-table">
            <thead>
              <tr>
                <th>ID</th>
                <th>タイトル</th>
                <th>ステータス</th>
                <th>開始時刻</th>
                <th>終了時刻</th>
                <th>所要時間</th>
              </tr>
            </thead>
            <tbody>
              {meetings.map((m) => (
                <tr key={m.id}>
                  <td>
                    <Link to={`/meetings/${m.id}`}>{m.id.slice(0, 8)}</Link>
                  </td>
                  <td>{m.title || "--"}</td>
                  <td>{getStatusLabel(m.status)}</td>
                  <td>{m.started_at ? formatDate(m.started_at) : "--"}</td>
                  <td>{m.stopped_at ? formatDate(m.stopped_at) : "--"}</td>
                  <td>
                    {m.duration_seconds != null 
                      ? formatDuration(m.duration_seconds) 
                      : "--"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          <div className="pagination">
            <button 
              disabled={page <= 1} 
              onClick={() => setPage(page - 1)}
            >
              前へ
            </button>
            <span>{page} / {totalPages}</span>
            <button 
              disabled={page >= totalPages} 
              onClick={() => setPage(page + 1)}
            >
              次へ
            </button>
          </div>
        </>
      )}
    </div>
  );
}
