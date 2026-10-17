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
  const [voiceChannelId, setVoiceChannelId] = useState("");
  const [reportChannelId, setReportChannelId] = useState("");
  const [startAt, setStartAt] = useState("");
  const [durationHours, setDurationHours] = useState("2");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    const [scheduleResponse, channelResponse] = await Promise.all([
      fetch("/api/guild/schedules"),
      fetch("/api/guild/meetings"),
    ]);
    if (scheduleResponse.ok) {
      const body = (await scheduleResponse.json()) as { schedules: Schedule[] };
      setSchedules(body.schedules);
    }
    if (channelResponse.ok) {
      const body = (await channelResponse.json()) as {
        voice_channels?: Array<{ id: string; label: string }>;
      };
      const available = (body.voice_channels ?? []).map((channel) => ({
        id: channel.id,
        name: channel.label,
      }));
      setChannels(available);
      setVoiceChannelId((current) => current || available[0]?.id || "");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function createSchedule(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setMessage("");
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
        setMessage(
          response.status === 409
            ? "同じサーバーに重なる予約があります。"
            : "予約を作成できませんでした。管理者権限と入力を確認してください。",
        );
        return;
      }
      setStartAt("");
      setMessage("毎週の予約を作成しました。");
      await load();
    } catch {
      setMessage("予約を作成できませんでした。");
    } finally {
      setBusy(false);
    }
  }

  async function cancelSchedule(id: string) {
    const response = await fetch(
      `/api/guild/schedules/${encodeURIComponent(id)}`,
      { method: "DELETE" },
    );
    if (response.ok) {
      setSchedules((items) => items.filter((item) => item.id !== id));
    } else {
      setMessage("予約を取り消せませんでした。");
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
        <h2>予約を追加</h2>
        <form className="settings-form" onSubmit={createSchedule}>
          <label>
            VC
            <select
              required
              value={voiceChannelId}
              onChange={(event) => setVoiceChannelId(event.target.value)}
            >
              {channels.map((channel) => (
                <option key={channel.id} value={channel.id}>
                  {channel.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            レポート先チャンネルID
            <input
              required
              value={reportChannelId}
              onChange={(event) => setReportChannelId(event.target.value)}
              placeholder="DiscordのテキストチャンネルID"
            />
          </label>
          <label>
            最初の開始日時
            <input
              required
              type="datetime-local"
              value={startAt}
              onChange={(event) => setStartAt(event.target.value)}
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
              onChange={(event) => setDurationHours(event.target.value)}
            />
          </label>
          <p>
            作成者の端末タイムゾーンで表示します。繰り返し時刻はUTCで固定され、夏時間の補正はありません。
          </p>
          <button type="submit" disabled={busy || !channels.length || !startAt}>
            {busy ? "保存中…" : "毎週の予約を作成"}
          </button>
          {message ? <p role="status">{message}</p> : null}
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
