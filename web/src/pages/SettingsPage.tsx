import { type FormEvent, useEffect, useState } from "react";
import { ForbiddenState } from "../components/ForbiddenState";
import {
  deleteGuildBotToken,
  fetchGuildSettings,
  updateGuildBotToken,
  updateGuildSettings,
} from "../lib/api";
import type {
  GuildSettingsResponse,
  UpdateGuildSettingsRequest,
} from "../lib/types";

interface SettingsForm {
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
    retention_raw_audio_ttl_days: readNumber(form.retention_raw_audio_ttl_days),
    retention_transcript_ttl_days: readNumber(
      form.retention_transcript_ttl_days,
    ),
    summary_enabled: form.summary_enabled,
  };
}

function readNumber(value: string): number {
  const parsed = Number(value);
  return Number.isInteger(parsed) ? parsed : Number.NaN;
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

function guildSettingsErrorMessage(err: unknown, fallback: string): string {
  if (!(err instanceof Error)) {
    return fallback;
  }
  switch (err.message) {
    case "forbidden":
      return "\u30ae\u30eb\u30c9\u7ba1\u7406\u6a29\u9650\u304c\u5fc5\u8981\u3067\u3059";
    case "invalid_bot_token":
      return "Discord Bot token \u304c\u7121\u52b9\u3067\u3059";
    case "not_bot_token":
      return "\u767b\u9332\u3067\u304d\u308b\u306e\u306f Bot token \u306e\u307f\u3067\u3059";
    case "bot_token_guild_access_denied":
      return "\u3053\u306e Bot token \u306f\u3053\u306e\u30ae\u30eb\u30c9\u306b\u30a2\u30af\u30bb\u30b9\u3067\u304d\u307e\u305b\u3093";
    case "missing_guild_bot_token_encryption_key":
      return "\u30b5\u30fc\u30d0\u30fc\u306b token \u6697\u53f7\u5316\u30ad\u30fc\u304c\u8a2d\u5b9a\u3055\u308c\u3066\u3044\u307e\u305b\u3093";
    default:
      return fallback;
  }
}

export function SettingsPage() {
  const [settings, setSettings] = useState<GuildSettingsResponse | null>(null);
  const [form, setForm] = useState<SettingsForm | null>(null);
  const [botTokenValue, setBotTokenValue] = useState("");
  const [loading, setLoading] = useState(true);
  const [activeOperation, setActiveOperation] = useState<
    "settings" | "token-save" | "token-delete" | null
  >(null);
  const [tokenDeleteConfirmPending, setTokenDeleteConfirmPending] =
    useState(false);
  const [error, setError] = useState<string | null>(null);
  const [forbidden, setForbidden] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    document.title = "\u30ae\u30eb\u30c9\u8a2d\u5b9a";
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setForbidden(false);
    setMessage(null);

    fetchGuildSettings(controller.signal)
      .then((settingsResponse) => {
        if (!controller.signal.aborted) {
          setSettings(settingsResponse);
          setForm(formFromSettings(settingsResponse));
        }
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        if (err instanceof Error && err.message === "forbidden") {
          setForbidden(true);
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

  const canEdit = settings?.is_admin ?? false;
  const isSavingAny = activeOperation !== null;
  const controlsDisabled = !canEdit || loading || isSavingAny || form == null;
  const tokenControlsDisabled =
    !canEdit || loading || isSavingAny || settings == null;

  function updateForm(update: Partial<SettingsForm>) {
    setForm((current) => (current ? { ...current, ...update } : current));
    setTokenDeleteConfirmPending(false);
    setError(null);
    setMessage(null);
  }

  function updateBotTokenValue(value: string) {
    setBotTokenValue(value);
    setTokenDeleteConfirmPending(false);
    setError(null);
    setMessage(null);
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!form || !canEdit || isSavingAny) {
      return;
    }

    setTokenDeleteConfirmPending(false);
    const validationError = validateForm(form);
    if (validationError) {
      setError(validationError);
      setMessage(null);
      return;
    }

    setActiveOperation("settings");
    setError(null);
    setMessage(null);

    try {
      const updated = await updateGuildSettings(requestFromForm(form));
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
      setActiveOperation(null);
    }
  }

  async function handleBotTokenSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!canEdit || isSavingAny) {
      return;
    }
    const token = botTokenValue.trim();
    if (!token) {
      setError(
        "Discord Bot token \u3092\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044",
      );
      setMessage(null);
      return;
    }

    setActiveOperation("token-save");
    setTokenDeleteConfirmPending(false);
    setError(null);
    setMessage(null);

    try {
      const updated = await updateGuildBotToken({ bot_token: token });
      setSettings(updated);
      setBotTokenValue("");
      setMessage(
        "Discord Bot token \u3092\u4fdd\u5b58\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      setError(
        guildSettingsErrorMessage(
          err,
          "Discord Bot token \u306e\u4fdd\u5b58\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      setActiveOperation(null);
    }
  }

  async function handleBotTokenDelete() {
    if (!canEdit || !settings?.discord_bot_token_registered || isSavingAny) {
      return;
    }
    if (!tokenDeleteConfirmPending) {
      setTokenDeleteConfirmPending(true);
      setError(null);
      setMessage(
        "Discord Bot token \u306e\u524a\u9664\u3092\u78ba\u8a8d\u3057\u3066\u304f\u3060\u3055\u3044",
      );
      return;
    }

    setActiveOperation("token-delete");
    setTokenDeleteConfirmPending(false);
    setError(null);
    setMessage(null);

    try {
      const updated = await deleteGuildBotToken();
      setSettings(updated);
      setBotTokenValue("");
      setMessage(
        "Discord Bot token \u306e\u30ae\u30eb\u30c9\u500b\u5225\u8a2d\u5b9a\u3092\u524a\u9664\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      setError(
        guildSettingsErrorMessage(
          err,
          "Discord Bot token \u306e\u524a\u9664\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      setTokenDeleteConfirmPending(false);
      setActiveOperation(null);
    }
  }

  if (!loading && forbidden) {
    return (
      <main className="settings-page">
        <div className="settings-header">
          <div>
            <h1>{"\u30ae\u30eb\u30c9\u8a2d\u5b9a"}</h1>
            <p>
              {
                "\u7ba1\u7406\u6a29\u9650\u304c\u3042\u308b\u30e6\u30fc\u30b6\u30fc\u306e\u307f\u8868\u793a\u3067\u304d\u307e\u3059"
              }
            </p>
          </div>
        </div>
        <ForbiddenState message="\u3053\u306e\u30ae\u30eb\u30c9\u8a2d\u5b9a\u3092\u8868\u793a\u3059\u308b\u6a29\u9650\u304c\u3042\u308a\u307e\u305b\u3093" />
      </main>
    );
  }

  return (
    <main className="settings-page">
      <div className="settings-header">
        <div>
          <h1>{"\u30ae\u30eb\u30c9\u8a2d\u5b9a"}</h1>
          <p>
            {
              "\u9332\u97f3\u3068\u8981\u7d04\u306e\u30c7\u30d5\u30a9\u30eb\u30c8\u52d5\u4f5c\u3092\u5909\u66f4\u3067\u304d\u307e\u3059"
            }
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
                step={1}
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
                step={1}
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
                step={1}
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
              {activeOperation === "settings"
                ? "\u4fdd\u5b58\u4e2d"
                : "\u4fdd\u5b58"}
            </button>
          </div>
        </form>
      ) : null}

      {settings ? (
        <section className="settings-section">
          <h2>{"Discord Bot"}</h2>
          <div className="settings-token-status-row">
            <span
              className={
                settings.discord_bot_token_registered
                  ? "settings-token-status is-set"
                  : "settings-token-status is-empty"
              }
            >
              {settings.discord_bot_token_registered
                ? "\u767b\u9332\u6e08\u307f"
                : "\u672a\u767b\u9332"}
            </span>
            {settings.discord_bot_username ? (
              <span className="settings-token-meta">
                {settings.discord_bot_username}
              </span>
            ) : null}
            {settings.discord_bot_token_last_validated_at ? (
              <span className="settings-token-meta">
                {"\u691c\u8a3c: "}
                {settings.discord_bot_token_last_validated_at}
              </span>
            ) : null}
            {settings.discord_bot_token_updated_at ? (
              <span className="settings-token-meta">
                {"\u66f4\u65b0: "}
                {settings.discord_bot_token_updated_at}
              </span>
            ) : null}
          </div>

          <form className="settings-token-form" onSubmit={handleBotTokenSubmit}>
            <label className="settings-field">
              <span>{"Bot token"}</span>
              <input
                type="password"
                autoComplete="new-password"
                value={botTokenValue}
                disabled={tokenControlsDisabled}
                onChange={(event) => updateBotTokenValue(event.target.value)}
              />
            </label>
            <div className="settings-token-actions">
              <button
                type="submit"
                className="primary-button"
                disabled={tokenControlsDisabled || botTokenValue.trim() === ""}
              >
                {activeOperation === "token-save"
                  ? "\u4fdd\u5b58\u4e2d"
                  : "\u66f4\u65b0"}
              </button>
              <button
                type="button"
                className="secondary-button"
                disabled={
                  tokenControlsDisabled ||
                  !settings.discord_bot_token_registered
                }
                onClick={handleBotTokenDelete}
              >
                {activeOperation === "token-delete"
                  ? "\u524a\u9664\u4e2d"
                  : tokenDeleteConfirmPending
                    ? "\u524a\u9664\u3092\u78ba\u5b9a"
                    : "\u524a\u9664"}
              </button>
            </div>
          </form>
        </section>
      ) : null}
    </main>
  );
}
