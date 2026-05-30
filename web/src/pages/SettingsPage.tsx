import { useEffect, useState } from "react";
import { fetchMe, fetchGuildSettings, updateGuildSettings } from "../lib/api";
import type { MeResponse, GuildSettingsResponse } from "../lib/types";

export function SettingsPage() {
  const [me, setMe] = useState<MeResponse | null>(null);
  const [settings, setSettings] = useState<GuildSettingsResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // Form state
  const [whisperLanguageExplicit, setWhisperLanguageExplicit] = useState(false);
  const [whisperLanguage, setWhisperLanguage] = useState("");
  const [whisperVad, setWhisperVad] = useState(true);
  const [autoStopGraceSeconds, setAutoStopGraceSeconds] = useState(60);
  const [retentionRawAudioTtlDays, setRetentionRawAudioTtlDays] = useState(7);
  const [retentionTranscriptTtlDays, setRetentionTranscriptTtlDays] = useState(30);
  const [summaryEnabled, setSummaryEnabled] = useState(true);

  useEffect(() => {
    const controller = new AbortController();
    
    Promise.all([fetchMe(controller.signal), fetchGuildSettings(controller.signal)])
      .then(([meRes, settingsRes]) => {
        setMe(meRes);
        setSettings(settingsRes);
        
        // Initialize form from settings
        if (settingsRes.whisper_language != null) {
          setWhisperLanguageExplicit(true);
          setWhisperLanguage(settingsRes.whisper_language);
        } else {
          setWhisperLanguageExplicit(false);
        }
        if (settingsRes.whisper_vad != null) setWhisperVad(settingsRes.whisper_vad);
        if (settingsRes.auto_stop_grace_seconds != null) 
          setAutoStopGraceSeconds(settingsRes.auto_stop_grace_seconds);
        if (settingsRes.retention_raw_audio_ttl_days != null) 
          setRetentionRawAudioTtlDays(settingsRes.retention_raw_audio_ttl_days);
        if (settingsRes.retention_transcript_ttl_days != null) 
          setRetentionTranscriptTtlDays(settingsRes.retention_transcript_ttl_days);
        if (settingsRes.summary_enabled != null) 
          setSummaryEnabled(settingsRes.summary_enabled);
      })
      .catch((err) => {
        if (err.name !== "AbortError") setError(err.message);
      })
      .finally(() => setLoading(false));

    return () => controller.abort();
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setSaveError(null);
    
    try {
      await updateGuildSettings({
        whisper_language: whisperLanguageExplicit 
          ? (whisperLanguage || null) 
          : undefined,
        whisper_language_explicit: whisperLanguageExplicit,
        whisper_vad,
        auto_stop_grace_seconds: autoStopGraceSeconds,
        retention_raw_audio_ttl_days: retentionRawAudioTtlDays,
        retention_transcript_ttl_days: retentionTranscriptTtlDays,
        summary_enabled,
      });
      
      // Refresh settings from server
      const updated = await fetchGuildSettings();
      setSettings(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (err: unknown) {
      setSaveError(err instanceof Error ? err.message : "保存に失敗しました");
    } finally {
      setSaving(false);
    }
  };

  if (loading) return <div className="loading-spinner">読み込み中...</div>;
  if (error) return <div className="dashboard-error">{error}</div>;

  const isAdmin = me?.is_guild_admin ?? false;

  return (
    <div className="settings-page">
      <h1>ギルド設定</h1>

      {!isAdmin && (
        <div className="notice">
          管理者のみ設定を変更できます。現在読み取り専用モードです。
        </div>
      )}

      {saveError && <div className="error-message">{saveError}</div>}
      {saved && <div className="success-message">保存しました</div>}

      <form className="settings-form">
        <fieldset disabled={!isAdmin || saving}>
          <legend>Whisper 設定</legend>
          
          <label>
            <input 
              type="checkbox" 
              checked={whisperLanguageExplicit}
              onChange={(e) => setWhisperLanguageExplicit(e.target.checked)}
            />
            言語を上書きする
          </label>

          {whisperLanguageExplicit && (
            <div className="form-group">
              <label>言語コード (ISO 639-1)</label>
              <input 
                type="text" 
                value={whisperLanguage}
                onChange={(e) => setWhisperLanguage(e.target.value)}
                placeholder="ja"
                maxLength={2}
              />
            </div>
          )}

          <label>
            <input 
              type="checkbox" 
              checked={whisperVad}
              onChange={(e) => setWhisperVad(e.target.checked)}
            />
            VAD (Voice Activity Detection) を有効にする
          </label>
        </fieldset>

        <fieldset disabled={!isAdmin || saving}>
          <legend>録画設定</legend>
          
          <div className="form-group">
            <label>自動停止の猶予時間 (秒, 10-3600)</label>
            <input 
              type="number" 
              min={10}
              max={3600}
              value={autoStopGraceSeconds}
              onChange={(e) => setAutoStopGraceSeconds(Number(e.target.value))}
            />
          </div>
        </fieldset>

        <fieldset disabled={!isAdmin || saving}>
          <legend>保持ポリシー</legend>
          
          <div className="form-group">
            <label>生オーディオの保持期間 (日, 1-365)</label>
            <input 
              type="number" 
              min={1}
              max={365}
              value={retentionRawAudioTtlDays}
              onChange={(e) => setRetentionRawAudioTtlDays(Number(e.target.value))}
            />
          </div>

          <div className="form-group">
            <label>文字起こしの保持期間 (日, 1-365)</label>
            <input 
              type="number" 
              min={1}
              max={365}
              value={retentionTranscriptTtlDays}
              onChange={(e) => setRetentionTranscriptTtlDays(Number(e.target.value))}
            />
          </div>
        </fieldset>

        <fieldset disabled={!isAdmin || saving}>
          <legend>要約設定</legend>
          
          <label>
            <input 
              type="checkbox" 
              checked={summaryEnabled}
              onChange={(e) => setSummaryEnabled(e.target.checked)}
            />
            要約を有効にする
          </label>
        </fieldset>

        {isAdmin && (
          <button type="button" onClick={handleSave} disabled={saving}>
            {saving ? "保存中..." : "保存する"}
          </button>
        )}
      </form>
    </div>
  );
}
