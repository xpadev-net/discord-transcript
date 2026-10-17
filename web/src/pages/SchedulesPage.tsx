import { useCallback, useEffect, useState } from "react";

type Schedule = {
  id: string;
  voice_channel_id: string;
  report_channel_id: string;
  next_start_at: string;
  local_start_time: string;
  timezone: string;
  duration_seconds: number;
  active: boolean;
};

type VoiceChannel = { id: string; name: string };

export function SchedulesPage() {
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [channels, setChannels] = useState<VoiceChannel[]>([]);
  const [reportChannels, setReportChannels] = useState<VoiceChannel[]>([]);
  const [voiceChannelId, setVoiceChannelId] = useState("");
  const [reportChannelId, setReportChannelId] = useState("");
  const [startAt, setStartAt] = useState("");
  const [durationHours, setDurationHours] = useState("2");
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setError("");
    try {
      const [scheduleResponse, channelResponse] = await Promise.all([
        fetch("/api/guild/schedules"),
        fetch("/api/guild/schedule-channels"),
      ]);
      if (!scheduleResponse.ok || !channelResponse.ok) {
        throw new Error(
          "録音開始権限とチャンネルへの閲覧権限を確認してください。",
        );
      }
      const body = (await scheduleResponse.json()) as { schedules: Schedule[] };
      setSchedules(body.schedules);
      const channelsBody = (await channelResponse.json()) as {
        voice_channels?: Array<{ id: string; name: string }>;
        report_channels?: Array<{ id: string; name: string }>;
      };
      const available = (channelsBody.voice_channels ?? []).map((channel) => ({
        id: channel.id,
        name: channel.name,
      }));
      setChannels(available);
      setVoiceChannelId((current) => current || available[0]?.id || "");
      const availableReports = channelsBody.report_channels ?? [];
      setReportChannels(availableReports);
      setReportChannelId((current) => current || availableReports[0]?.id || "");
    } catch (loadError) {
      setError(
        loadError instanceof Error
          ? loadError.message
          : "予約情報を読み込めませんでした。通信状態を確認して再試行してください。",
      );
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function createSchedule(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setMessage("");
    setError("");
    try {
      const start = new Date(startAt);
      const response = await fetch("/api/guild/schedules", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          voice_channel_id: voiceChannelId,
          report_channel_id: reportChannelId,
          next_start_at: start.toISOString(),
          local_start_time: `${String(start.getHours()).padStart(2, "0")}:${String(start.getMinutes()).padStart(2, "0")}`,
          timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
          duration_seconds: Math.round(Number(durationHours) * 3600),
        }),
      });
      if (!response.ok) {
        setError(
          response.status === 409
            ? "同じサーバーに重なる予約があります。"
            : "予約を作成できませんでした。録音開始権限と入力を確認してください。",
        );
        return;
      }
      setStartAt("");
      setMessage("毎週の予約を作成しました。");
      await load();
    } catch {
      setError("予約を作成できませんでした。通信状態を確認してください。");
    } finally {
      setBusy(false);
    }
  }

  async function cancelSchedule(id: string) {
    setError("");
    try {
      const response = await fetch(
        `/api/guild/schedules/${encodeURIComponent(id)}`,
        { method: "DELETE" },
      );
      if (!response.ok) {
        throw new Error(
          "予約を取り消せませんでした。録音開始権限を確認してください。",
        );
      }
      setSchedules((items) => items.filter((item) => item.id !== id));
    } catch (cancelError) {
      setError(
        cancelError instanceof Error
          ? cancelError.message
          : "予約を取り消せませんでした。通信状態を確認してください。",
      );
    }
  }

  return (
    <main className="page-content">
      <header className="page-header">
        <div>
          <p className="eyebrow">Recording</p>
          <h1>会議の予約</h1>
          <p>
            毎週の時間枠を登録します。VCに人間が2人以上いると録音を開始します。
          </p>
        </div>
      </header>
      <section className="settings-section">
        <div className="section-heading">
          <h2>予約を追加</h2>
          <button type="button" onClick={() => void load()}>
            一覧を再読み込み
          </button>
        </div>
        <form className="settings-form" onSubmit={createSchedule}>
          <label>
            VC
            <select
              required
              value={voiceChannelId}
              onChange={(event) => {
                setError("");
                setVoiceChannelId(event.target.value);
              }}
            >
              {channels.map((channel) => (
                <option key={channel.id} value={channel.id}>
                  {channel.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            レポート先チャンネル
            <select
              required
              value={reportChannelId}
              onChange={(event) => {
                setError("");
                setReportChannelId(event.target.value);
              }}
            >
              {reportChannels.map((channel) => (
                <option key={channel.id} value={channel.id}>
                  {channel.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            最初の開始日時
            <input
              required
              type="datetime-local"
              value={startAt}
              onChange={(event) => {
                setError("");
                setStartAt(event.target.value);
              }}
            />
          </label>
          <label>
            時間枠（時間）
            <input
              required
              type="number"
              min="1"
              max="24"
              step="0.5"
              value={durationHours}
              onChange={(event) => {
                setError("");
                setDurationHours(event.target.value);
              }}
            />
          </label>
          <p>
            作成者の端末タイムゾーンで表示します。繰り返し時刻はUTCで固定され、夏時間の補正はありません。
          </p>
          <button
            type="submit"
            disabled={
              busy || !channels.length || !reportChannels.length || !startAt
            }
          >
            {busy ? "保存中…" : "毎週の予約を作成"}
          </button>
          {message ? <p role="status">{message}</p> : null}
          {error ? <p role="alert">{error}</p> : null}
        </form>
      </section>
      <section className="settings-section">
        <h2>有効な予約</h2>
        {schedules.length ? (
          <ul className="settings-list">
            {schedules.map((schedule) => (
              <li key={schedule.id}>
                <div>
                  <strong>
                    {new Date(schedule.next_start_at).toLocaleString()}
                  </strong>
                  <p>
                    VC:{" "}
                    {channels.find(
                      (channel) => channel.id === schedule.voice_channel_id,
                    )?.name ?? schedule.voice_channel_id}{" "}
                    · {schedule.duration_seconds / 3600}時間 · 毎週
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => void cancelSchedule(schedule.id)}
                >
                  取り消す
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p>有効な予約はありません。</p>
        )}
      </section>
    </main>
  );
}
