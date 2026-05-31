import { type FormEvent, useEffect, useState } from "react";
import { fetchGuildSettings, fetchMe, updateGuildSettings } from "../lib/api";
import type {
  GuildSettingsResponse,
  MeResponse,
  UpdateGuildSettingsRequest,
} from "../lib/types";

interface SettingsForm {
  whisper_language: string | null;
  whisper_vad: boolean;
  auto_stop_grace_seconds: string;
  retention_raw_audio_ttl_days: string;
  retention_transcript_ttl_days: string;
  summary_enabled: boolean;
  whisper_language_enabled: boolean;
  whisper_language_value: string;
}

function formFromSettings(settings: GuildSettingsResponse): SettingsForm {
  return {
    whisper_language: settings.whisper_language,
    whisper_language_enabled: settings.whisper_language_explicit,
    whisper_language_value: settings.whisper_language ?? "",
    whisper_vad: settings.whisper_vad,
    auto_stop_grace_seconds: String(settings.auto_stop_grace_seconds),
    retention_raw_audio_ttl_days: String(settings.retention_raw_audio_ttl_days),
    retention_transcript_ttl_days: String(
      settings.retention_transcript_ttl_days,
    ),
    summary_enabled: settings.summary_enabled,
  };
}

function requestFromForm(form: SettingsForm): UpdateGuildSettingsRequest {
  const language = form.whisper_language_enabled
    ? form.whisper_language_value.trim().toLowerCase()
    : null;

  return {
    whisper_language: language === "" ? null : language,
    whisper_vad: form.whisper_vad,
    auto_stop_grace_seconds: readNumber(form.auto_stop_grace_seconds),
    retention_raw_audio_ttl_days: readNumber(
      form.retention_raw_audio_ttl_days,
    ),
    retention_transcript_ttl_days: readNumber(
      form.retention_transcript_ttl_days,
    ),
    summary_enabled: form.summary_enabled,
  };
}

function readNumber(value: string): number {
  return Number.parseInt(value, 10);
}

function validateForm(form: SettingsForm): string | null {
  const language = form.whisper_language_enabled
    ? form.whisper_language_value.trim().toLowerCase()
    : null;

  if (form.whisper_language_enabled && !language) {
    return "\u8a00\u8a9e\u3092\u6307\u5b9a\u3059\u308b\u5834\u5408\u306f2\u6587\u5b57\u306e\u8a00\u8a9e\u30b3\u30fc\u30c9\u304c\u5fc5\u8981\u3067\u3059";
  }
  if (language && !/^[a-z]{2}$/.test(language)) {
    return "\u8a00\u8a9e\u30b3\u30fc\u30c9\u306fISO 639-1\u306e2\u6587\u5b57\u3067\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  const autoStopGraceSeconds = readNumber(form.auto_stop_grace_seconds);
  if (
    !Number.isFinite(autoStopGraceSeconds) ||
    autoStopGraceSeconds < 10 ||
    autoStopGraceSeconds > 3600
  ) {
    return "\u81ea\u52d5\u505c\u6b62\u307e\u3067\u306e\u79d2\u6570\u306f10\u304b\u30893600\u3067\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  const rawAudioTtlDays = readNumber(form.retention_raw_audio_ttl_days);
  const transcriptTtlDays = readNumber(form.retention_transcript_ttl_days);
  if (
    !Number.isFinite(rawAudioTtlDays) ||
    !Number.isFinite(transcriptTtlDays) ||
    rawAudioTtlDays < 1 ||
    rawAudioTtlDays > 365 ||
    transcriptTtlDays < 1 ||
    transcriptTtlDays > 365
  ) {
    return "\u4fdd\u6301\u65e5\u6570\u306f1\u304b\u3089365\u3067\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }

  return null;
}

export function SettingsPage() {
  const [me, setMe] = useState<MeResponse | null>(null);
  const [settings, setSettings] = useState<GuildSettingsResponse | null>(null);
  const [form, setForm] = useState<SettingsForm | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    document.title = "\u30ae\u30eb\u30c9\u8a2d\u5b9a";
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setMessage(null);

    Promise.all([
      fetchMe(controller.signal),
      fetchGuildSettings(controller.signal),
    ])
      .then(([meResponse, settingsResponse]) => {
        if (!controller.signal.aborted) {
          setMe(meResponse);
          setSettings(settingsResponse);
          setForm(formFromSettings(settingsResponse));
        }
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        setError(
          err instanceof Error
            ? err.message
            : "\u8a2d\u5b9a\u306e\u8aad\u307f\u8fbc\u307f\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        );
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoading(false);
        }
      });

    return () => controller.abort();
  }, []);

  const canEdit = settings?.is_admin ?? me?.is_admin ?? false;
  const controlsDisabled = !canEdit || loading || saving || form == null;
  const hasWhisperVad = settings?.whisper_vad != null;

  function updateForm(update: Partial<SettingsForm>) {
    setForm((current) => (current ? { ...current, ...update } : current));
    setMessage(null);
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!form || !canEdit) {
      return;
    }

    const validationError = validateForm(form);
    if (validationError) {
      setError(validationError);
      setMessage(null);
      return;
    }

    const controller = new AbortController();
    setSaving(true);
    setError(null);
    setMessage(null);

    try {
      const updated = await updateGuildSettings(
        requestFromForm(form),
        controller.signal,
      );
      setSettings(updated);
      setForm(formFromSettings(updated));
      setMessage("\u8a2d\u5b9a\u3092\u4fdd\u5b58\u3057\u307e\u3057\u305f");
    } catch (err) {
      const text =
        err instanceof Error && err.message === "forbidden"
          ? "\u7ba1\u7406\u6a29\u9650\u304c\u306a\u3044\u305f\u3081\u4fdd\u5b58\u3067\u304d\u307e\u305b\u3093"
          : "\u8a2d\u5b9a\u306e\u4fdd\u5b58\u306b\u5931\u6557\u3057\u307e\u3057\u305f";
      setError(text);
    } finally {
      setSaving(false);
    }
  }

  return (
    <main className="settings-page">
      <div className="settings-header">
        <div>
          <h1>{"\u30ae\u30eb\u30c9\u8a2d\u5b9a"}</h1>
          <p>
            {canEdit
              ? "\u9332\u97f3\u3068\u8981\u7d04\u306e\u30c7\u30d5\u30a9\u30eb\u30c8\u52d5\u4f5c\u3092\u5909\u66f4\u3067\u304d\u307e\u3059"
              : "\u8a2d\u5b9a\u306f\u8868\u793a\u306e\u307f\u3067\u3059"}
          </p>
        </div>
      </div>

      {loading ? (
        <output className="loading settings-panel-message">
          <span className="loading-spinner" />
          {"\u8aad\u307f\u8fbc\u307f\u4e2d"}
        </output>
      ) : null}

      {error ? (
        <div className="panel-error settings-panel-message" role="alert">
          {error}
        </div>
      ) : null}

      {!canEdit && settings ? (
        <div className="settings-notice" role="note">
          {
            "\u3053\u306e\u8a2d\u5b9a\u306e\u5909\u66f4\u306b\u306f\u30ae\u30eb\u30c9\u7ba1\u7406\u6a29\u9650\u304c\u5fc5\u8981\u3067\u3059"
          }
        </div>
      ) : null}

      {message ? <output className="settings-success">{message}</output> : null}

      {form ? (
        <form className="settings-form" onSubmit={handleSubmit}>
          <section className="settings-section">
            <h2>{"\u8981\u7d04"}</h2>
            <label className="settings-checkbox">
              <input
                type="checkbox"
                checked={form.summary_enabled}
                disabled={controlsDisabled}
                onChange={(event) =>
                  updateForm({ summary_enabled: event.target.checked })
                }
              />
              <span>{"\u8981\u7d04\u3092\u6709\u52b9\u306b\u3059\u308b"}</span>
            </label>
          </section>

          <section className="settings-section">
            <h2>{"\u97f3\u58f0\u8a8d\u8b58"}</h2>
            <label className="settings-checkbox">
              <input
                type="checkbox"
                checked={form.whisper_language_enabled}
                disabled={controlsDisabled}
                onChange={(event) =>
                  updateForm({
                    whisper_language_enabled: event.target.checked,
                  })
                }
              />
              <span>{"\u8a00\u8a9e\u3092\u6307\u5b9a\u3059\u308b"}</span>
            </label>
            <label className="settings-field">
              <span>{"\u8a00\u8a9e\u30b3\u30fc\u30c9"}</span>
              <input
                type="text"
                inputMode="text"
                maxLength={2}
                pattern="[A-Za-z]{2}"
                placeholder="ja"
                value={form.whisper_language_value}
                disabled={controlsDisabled || !form.whisper_language_enabled}
                onChange={(event) =>
                  updateForm({ whisper_language_value: event.target.value })
                }
              />
            </label>
            {hasWhisperVad ? (
              <label className="settings-checkbox">
                <input
                  type="checkbox"
                  checked={form.whisper_vad}
                  disabled={controlsDisabled}
                  onChange={(event) =>
                    updateForm({ whisper_vad: event.target.checked })
                  }
                />
                <span>{"Whisper VAD \u3092\u4f7f\u7528\u3059\u308b"}</span>
              </label>
            ) : null}
          </section>

          <section className="settings-section">
            <h2>{"\u81ea\u52d5\u505c\u6b62\u3068\u4fdd\u6301"}</h2>
            <label className="settings-field">
              <span>
                {"\u81ea\u52d5\u505c\u6b62\u307e\u3067\u306e\u79d2\u6570"}
              </span>
              <input
                type="number"
                required
                min={10}
                max={3600}
                value={form.auto_stop_grace_seconds}
                disabled={controlsDisabled}
                onChange={(event) =>
                  updateForm({
                    auto_stop_grace_seconds: event.target.value,
                  })
                }
              />
            </label>
            <label className="settings-field">
              <span>
                {"\u97f3\u58f0\u30d5\u30a1\u30a4\u30eb\u4fdd\u6301\u65e5\u6570"}
              </span>
              <input
                type="number"
                required
                min={1}
                max={365}
                value={form.retention_raw_audio_ttl_days}
                disabled={controlsDisabled}
                onChange={(event) =>
                  updateForm({
                    retention_raw_audio_ttl_days: event.target.value,
                  })
                }
              />
            </label>
            <label className="settings-field">
              <span>
                {"\u6587\u5b57\u8d77\u3053\u3057\u4fdd\u6301\u65e5\u6570"}
              </span>
              <input
                type="number"
                required
                min={1}
                max={365}
                value={form.retention_transcript_ttl_days}
                disabled={controlsDisabled}
                onChange={(event) =>
                  updateForm({
                    retention_transcript_ttl_days: event.target.value,
                  })
                }
              />
            </label>
          </section>

          <div className="settings-actions">
            <button
              type="submit"
              className="primary-button"
              disabled={controlsDisabled}
            >
              {saving ? "\u4fdd\u5b58\u4e2d" : "\u4fdd\u5b58"}
            </button>
          </div>
        </form>
      ) : null}
    </main>
  );
}
