import { type FormEvent, useCallback, useEffect, useState } from "react";
import { ForbiddenState } from "../components/ForbiddenState";
import {
  activateDomainKnowledgeItem,
  activateSummaryTemplate,
  archiveDomainKnowledgeItem,
  archiveSummaryTemplate,
  createDomainKnowledgeItem,
  createSummaryTemplate,
  deleteGuildBotToken,
  fetchDomainKnowledgeItems,
  fetchGuildSettings,
  fetchSummaryTemplates,
  updateDomainKnowledgeItem,
  updateGuildBotToken,
  updateGuildSettings,
  updateSummaryTemplate,
} from "../lib/api";
import type {
  DomainKnowledgeContentType,
  DomainKnowledgeItem,
  DomainKnowledgeUpsertRequest,
  GuildSettingsResponse,
  SummaryTemplate,
  SummaryTemplateUpsertRequest,
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

type ActiveOperation =
  | "settings"
  | "token-save"
  | "token-delete"
  | "domain-save"
  | "domain-activate"
  | "domain-archive"
  | "template-save"
  | "template-activate"
  | "template-archive";

interface DomainKnowledgeDraft {
  id: string | null;
  content_type: DomainKnowledgeContentType;
  title: string;
  body: string;
  active: boolean;
}

interface SummaryTemplateDraft {
  id: string | null;
  name: string;
  template: string;
  active: boolean;
}

const domainKnowledgeContentTypes: DomainKnowledgeContentType[] = [
  "glossary",
  "person_name",
  "project_context",
  "wording_rule",
  "prohibited_item",
];

const domainKnowledgeTypeLabels: Record<DomainKnowledgeContentType, string> = {
  glossary: "\u7528\u8a9e\u96c6",
  person_name: "\u4eba\u540d",
  project_context: "\u30d7\u30ed\u30b8\u30a7\u30af\u30c8\u60c5\u5831",
  wording_rule: "\u8868\u8a18\u30eb\u30fc\u30eb",
  prohibited_item: "\u7981\u6b62\u9805\u76ee",
};

const allowedSummaryTemplateVariables = new Set([
  "transcript_path",
  "manifest_path",
  "language",
  "speaker_roster",
  "domain_context_path",
]);
const textEncoder = new TextEncoder();

function utf8ByteLength(value: string): number {
  return textEncoder.encode(value).length;
}

function emptyDomainKnowledgeDraft(): DomainKnowledgeDraft {
  return {
    id: null,
    content_type: "glossary",
    title: "",
    body: "",
    active: true,
  };
}

function emptySummaryTemplateDraft(): SummaryTemplateDraft {
  return {
    id: null,
    name: "",
    template: "",
    active: true,
  };
}

function domainKnowledgeDraftFromItem(
  item: DomainKnowledgeItem,
): DomainKnowledgeDraft {
  return {
    id: item.id,
    content_type: item.content_type,
    title: item.title,
    body: item.body,
    active: item.active,
  };
}

function summaryTemplateDraftFromItem(
  item: SummaryTemplate,
): SummaryTemplateDraft {
  return {
    id: item.id,
    name: item.name,
    template: item.template,
    active: item.active,
  };
}

function chooseDomainKnowledgeDraft(
  items: DomainKnowledgeItem[],
  preferredId?: string | null,
): DomainKnowledgeDraft {
  const selected =
    (preferredId ? items.find((item) => item.id === preferredId) : null) ??
    items.find((item) => item.active && item.archived_at == null) ??
    items.find((item) => item.archived_at == null) ??
    items[0];
  return selected
    ? domainKnowledgeDraftFromItem(selected)
    : emptyDomainKnowledgeDraft();
}

function chooseSummaryTemplateDraft(
  items: SummaryTemplate[],
  preferredId?: string | null,
): SummaryTemplateDraft {
  const selected =
    (preferredId ? items.find((item) => item.id === preferredId) : null) ??
    items.find((item) => item.active && item.archived_at == null) ??
    items.find((item) => item.archived_at == null) ??
    items[0];
  return selected
    ? summaryTemplateDraftFromItem(selected)
    : emptySummaryTemplateDraft();
}

function domainKnowledgeRequestFromDraft(
  draft: DomainKnowledgeDraft,
): DomainKnowledgeUpsertRequest {
  return {
    content_type: draft.content_type,
    title: draft.title.trim(),
    body: draft.body.trim(),
    active: draft.active,
  };
}

function summaryTemplateRequestFromDraft(
  draft: SummaryTemplateDraft,
): SummaryTemplateUpsertRequest {
  return {
    name: draft.name.trim(),
    template: draft.template.trim(),
    active: draft.active,
  };
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

function validateDomainKnowledgeDraft(
  draft: DomainKnowledgeDraft,
): string | null {
  const title = draft.title.trim();
  const body = draft.body.trim();
  if (!title) {
    return "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306e\u30bf\u30a4\u30c8\u30eb\u3092\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  if (utf8ByteLength(title) > 200) {
    return "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306e\u30bf\u30a4\u30c8\u30eb\u306fUTF-8\u3067200\u30d0\u30a4\u30c8\u4ee5\u5185\u3067\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  if (!body) {
    return "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306e\u672c\u6587\u3092\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  if (utf8ByteLength(body) > 20_000) {
    return "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306e\u672c\u6587\u306fUTF-8\u306720000\u30d0\u30a4\u30c8\u4ee5\u5185\u3067\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  return null;
}

function validateSummaryTemplateDraft(
  draft: SummaryTemplateDraft,
): string | null {
  const name = draft.name.trim();
  const template = draft.template.trim();
  if (!name) {
    return "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u540d\u3092\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  if (utf8ByteLength(name) > 200) {
    return "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u540d\u306fUTF-8\u3067200\u30d0\u30a4\u30c8\u4ee5\u5185\u3067\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  if (!template) {
    return "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u672c\u6587\u3092\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  if (utf8ByteLength(template) > 20_000) {
    return "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u672c\u6587\u306fUTF-8\u306720000\u30d0\u30a4\u30c8\u4ee5\u5185\u3067\u5165\u529b\u3057\u3066\u304f\u3060\u3055\u3044";
  }
  return validateSummaryTemplateVariables(template);
}

function validateSummaryTemplateVariables(template: string): string | null {
  let rest = template;
  while (true) {
    const start = rest.indexOf("{{");
    if (start === -1) {
      return null;
    }
    const afterStart = rest.slice(start + 2);
    const end = afterStart.indexOf("}}");
    if (end === -1) {
      return "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u306e\u5909\u6570\u304c\u9589\u3058\u3089\u308c\u3066\u3044\u307e\u305b\u3093";
    }
    const name = afterStart.slice(0, end).trim();
    if (!name) {
      return "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u306e\u5909\u6570\u540d\u304c\u7a7a\u3067\u3059";
    }
    if (!allowedSummaryTemplateVariables.has(name)) {
      return `\u4f7f\u7528\u3067\u304d\u306a\u3044\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u5909\u6570\u3067\u3059: ${name}`;
    }
    rest = afterStart.slice(end + 2);
  }
}

function customizationErrorMessage(err: unknown, fallback: string): string {
  if (err instanceof Error) {
    switch (err.message) {
      case "forbidden":
      case "403 Forbidden":
        return "\u7ba1\u7406\u6a29\u9650\u304c\u306a\u3044\u305f\u3081\u64cd\u4f5c\u3067\u304d\u307e\u305b\u3093";
      case "400 Bad Request":
        return "\u5165\u529b\u5185\u5bb9\u304c\u30b5\u30fc\u30d0\u30fc\u306e\u691c\u8a3c\u306b\u901a\u308a\u307e\u305b\u3093\u3067\u3057\u305f";
      case "404 Not Found":
        return "\u5bfe\u8c61\u306e\u30d0\u30fc\u30b8\u30e7\u30f3\u304c\u898b\u3064\u304b\u308a\u307e\u305b\u3093";
      case "409 Conflict":
        return "\u5225\u306e\u6709\u52b9\u306a\u30d0\u30fc\u30b8\u30e7\u30f3\u3068\u7af6\u5408\u3057\u307e\u3057\u305f\u3002\u8aad\u307f\u8fbc\u307f\u76f4\u3057\u3066\u304b\u3089\u518d\u5b9f\u884c\u3057\u3066\u304f\u3060\u3055\u3044";
      default:
        return fallback;
    }
  }
  return fallback;
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
  const [domainKnowledgeItems, setDomainKnowledgeItems] = useState<
    DomainKnowledgeItem[]
  >([]);
  const [summaryTemplates, setSummaryTemplates] = useState<SummaryTemplate[]>(
    [],
  );
  const [domainKnowledgeDraft, setDomainKnowledgeDraft] =
    useState<DomainKnowledgeDraft>(emptyDomainKnowledgeDraft);
  const [summaryTemplateDraft, setSummaryTemplateDraft] =
    useState<SummaryTemplateDraft>(emptySummaryTemplateDraft);
  const [botTokenValue, setBotTokenValue] = useState("");
  const [loading, setLoading] = useState(true);
  const [customizationLoading, setCustomizationLoading] = useState(false);
  const [activeOperation, setActiveOperation] =
    useState<ActiveOperation | null>(null);
  const [tokenDeleteConfirmPending, setTokenDeleteConfirmPending] =
    useState(false);
  const [error, setError] = useState<string | null>(null);
  const [customizationError, setCustomizationError] = useState<string | null>(
    null,
  );
  const [forbidden, setForbidden] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    document.title = "\u30ae\u30eb\u30c9\u8a2d\u5b9a";
  }, []);

  const refreshCustomizations = useCallback(
    async (
      signal?: AbortSignal,
      preferredDomainKnowledgeId?: string | null,
      preferredSummaryTemplateId?: string | null,
    ) => {
      setCustomizationLoading(true);
      setCustomizationError(null);

      try {
        const [domainItems, templates] = await Promise.all([
          fetchDomainKnowledgeItems({ includeArchived: true }, signal),
          fetchSummaryTemplates({ includeArchived: true }, signal),
        ]);
        if (signal?.aborted) {
          return;
        }
        setDomainKnowledgeItems(domainItems);
        setSummaryTemplates(templates);
        setDomainKnowledgeDraft((current) =>
          chooseDomainKnowledgeDraft(
            domainItems,
            preferredDomainKnowledgeId === undefined
              ? current.id
              : preferredDomainKnowledgeId,
          ),
        );
        setSummaryTemplateDraft((current) =>
          chooseSummaryTemplateDraft(
            templates,
            preferredSummaryTemplateId === undefined
              ? current.id
              : preferredSummaryTemplateId,
          ),
        );
      } catch (err) {
        if (signal?.aborted) {
          return;
        }
        setCustomizationError(
          customizationErrorMessage(
            err,
            "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u3068\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u306e\u8aad\u307f\u8fbc\u307f\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
          ),
        );
      } finally {
        if (!signal?.aborted) {
          setCustomizationLoading(false);
        }
      }
    },
    [],
  );

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setCustomizationError(null);
    setForbidden(false);
    setMessage(null);

    fetchGuildSettings(controller.signal)
      .then((settingsResponse) => {
        if (!controller.signal.aborted) {
          setSettings(settingsResponse);
          setForm(formFromSettings(settingsResponse));
          if (settingsResponse.is_admin) {
            void refreshCustomizations(controller.signal);
          }
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
  }, [refreshCustomizations]);

  const canEdit = settings?.is_admin ?? false;
  const isSavingAny = activeOperation !== null;
  const controlsDisabled = !canEdit || loading || isSavingAny || form == null;
  const tokenControlsDisabled =
    !canEdit || loading || isSavingAny || settings == null;
  const selectedDomainKnowledgeItem = domainKnowledgeDraft.id
    ? (domainKnowledgeItems.find(
        (item) => item.id === domainKnowledgeDraft.id,
      ) ?? null)
    : null;
  const selectedSummaryTemplate = summaryTemplateDraft.id
    ? (summaryTemplates.find((item) => item.id === summaryTemplateDraft.id) ??
      null)
    : null;
  const activeDomainKnowledgeItems = domainKnowledgeItems.filter(
    (item) => item.active && item.archived_at == null,
  );
  const activeSummaryTemplate = summaryTemplates.find(
    (item) => item.active && item.archived_at == null,
  );
  const customizationControlsDisabled =
    !canEdit || customizationLoading || isSavingAny;

  function updateForm(update: Partial<SettingsForm>) {
    setForm((current) => (current ? { ...current, ...update } : current));
    setTokenDeleteConfirmPending(false);
    setError(null);
    setMessage(null);
  }

  function updateDomainKnowledgeDraft(update: Partial<DomainKnowledgeDraft>) {
    setDomainKnowledgeDraft((current) => ({ ...current, ...update }));
    setCustomizationError(null);
    setMessage(null);
  }

  function updateSummaryTemplateDraft(update: Partial<SummaryTemplateDraft>) {
    setSummaryTemplateDraft((current) => ({ ...current, ...update }));
    setCustomizationError(null);
    setMessage(null);
  }

  function handleDomainKnowledgeSelect(itemId: string) {
    const selected = domainKnowledgeItems.find((item) => item.id === itemId);
    setDomainKnowledgeDraft(
      selected
        ? domainKnowledgeDraftFromItem(selected)
        : emptyDomainKnowledgeDraft(),
    );
    setCustomizationError(null);
    setMessage(null);
  }

  function handleSummaryTemplateSelect(templateId: string) {
    const selected = summaryTemplates.find((item) => item.id === templateId);
    setSummaryTemplateDraft(
      selected
        ? summaryTemplateDraftFromItem(selected)
        : emptySummaryTemplateDraft(),
    );
    setCustomizationError(null);
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

  async function handleDomainKnowledgeSave(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!canEdit || customizationControlsDisabled) {
      return;
    }
    const validationError = validateDomainKnowledgeDraft(domainKnowledgeDraft);
    if (validationError) {
      setCustomizationError(validationError);
      setMessage(null);
      return;
    }
    if (selectedDomainKnowledgeItem?.archived_at) {
      setCustomizationError(
        "\u30a2\u30fc\u30ab\u30a4\u30d6\u6e08\u307f\u306e\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306f\u4fdd\u5b58\u3067\u304d\u307e\u305b\u3093",
      );
      setMessage(null);
      return;
    }

    setActiveOperation("domain-save");
    setCustomizationError(null);
    setMessage(null);

    try {
      const request = domainKnowledgeRequestFromDraft(domainKnowledgeDraft);
      const updated = domainKnowledgeDraft.id
        ? await updateDomainKnowledgeItem(domainKnowledgeDraft.id, request)
        : await createDomainKnowledgeItem(request);
      await refreshCustomizations(
        undefined,
        updated.id,
        summaryTemplateDraft.id,
      );
      setMessage(
        "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u3092\u4fdd\u5b58\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      setCustomizationError(
        customizationErrorMessage(
          err,
          "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306e\u4fdd\u5b58\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      setActiveOperation(null);
    }
  }

  async function handleDomainKnowledgeActivate() {
    if (
      !canEdit ||
      customizationControlsDisabled ||
      !selectedDomainKnowledgeItem
    ) {
      return;
    }

    setActiveOperation("domain-activate");
    setCustomizationError(null);
    setMessage(null);

    try {
      const updated = await activateDomainKnowledgeItem(
        selectedDomainKnowledgeItem.id,
      );
      await refreshCustomizations(
        undefined,
        updated.id,
        summaryTemplateDraft.id,
      );
      setMessage(
        "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u3092\u6709\u52b9\u5316\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      setCustomizationError(
        customizationErrorMessage(
          err,
          "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306e\u6709\u52b9\u5316\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      setActiveOperation(null);
    }
  }

  async function handleDomainKnowledgeArchive() {
    if (
      !canEdit ||
      customizationControlsDisabled ||
      !selectedDomainKnowledgeItem ||
      selectedDomainKnowledgeItem.archived_at
    ) {
      return;
    }

    setActiveOperation("domain-archive");
    setCustomizationError(null);
    setMessage(null);

    try {
      const updated = await archiveDomainKnowledgeItem(
        selectedDomainKnowledgeItem.id,
      );
      await refreshCustomizations(
        undefined,
        updated.id,
        summaryTemplateDraft.id,
      );
      setMessage(
        "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u3092\u30a2\u30fc\u30ab\u30a4\u30d6\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      setCustomizationError(
        customizationErrorMessage(
          err,
          "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306e\u30a2\u30fc\u30ab\u30a4\u30d6\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      setActiveOperation(null);
    }
  }

  async function handleSummaryTemplateSave(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!canEdit || customizationControlsDisabled) {
      return;
    }
    const validationError = validateSummaryTemplateDraft(summaryTemplateDraft);
    if (validationError) {
      setCustomizationError(validationError);
      setMessage(null);
      return;
    }
    if (selectedSummaryTemplate?.archived_at) {
      setCustomizationError(
        "\u30a2\u30fc\u30ab\u30a4\u30d6\u6e08\u307f\u306e\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u306f\u4fdd\u5b58\u3067\u304d\u307e\u305b\u3093",
      );
      setMessage(null);
      return;
    }

    setActiveOperation("template-save");
    setCustomizationError(null);
    setMessage(null);

    try {
      const request = summaryTemplateRequestFromDraft(summaryTemplateDraft);
      const updated = summaryTemplateDraft.id
        ? await updateSummaryTemplate(summaryTemplateDraft.id, request)
        : await createSummaryTemplate(request);
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        updated.id,
      );
      setMessage(
        "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u3092\u4fdd\u5b58\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      setCustomizationError(
        customizationErrorMessage(
          err,
          "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u306e\u4fdd\u5b58\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      setActiveOperation(null);
    }
  }

  async function handleSummaryTemplateActivate() {
    if (!canEdit || customizationControlsDisabled || !selectedSummaryTemplate) {
      return;
    }

    setActiveOperation("template-activate");
    setCustomizationError(null);
    setMessage(null);

    try {
      const updated = await activateSummaryTemplate(selectedSummaryTemplate.id);
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        updated.id,
      );
      setMessage(
        "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u3092\u6709\u52b9\u5316\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      setCustomizationError(
        customizationErrorMessage(
          err,
          "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u306e\u6709\u52b9\u5316\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      setActiveOperation(null);
    }
  }

  async function handleSummaryTemplateArchive() {
    if (
      !canEdit ||
      customizationControlsDisabled ||
      !selectedSummaryTemplate ||
      selectedSummaryTemplate.archived_at
    ) {
      return;
    }

    setActiveOperation("template-archive");
    setCustomizationError(null);
    setMessage(null);

    try {
      const updated = await archiveSummaryTemplate(selectedSummaryTemplate.id);
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        updated.id,
      );
      setMessage(
        "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u3092\u30a2\u30fc\u30ab\u30a4\u30d6\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      setCustomizationError(
        customizationErrorMessage(
          err,
          "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u306e\u30a2\u30fc\u30ab\u30a4\u30d6\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
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
        <section className="settings-section settings-customization-section">
          <div className="settings-section-heading">
            <div>
              <h2>{"AI \u30ab\u30b9\u30bf\u30de\u30a4\u30ba"}</h2>
              <p>
                {
                  "\u8981\u7d04\u306b\u4f7f\u3046\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u3068\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u3092\u7ba1\u7406\u3057\u307e\u3059"
                }
              </p>
            </div>
            <button
              type="button"
              className="secondary-button"
              disabled={customizationControlsDisabled}
              onClick={() =>
                void refreshCustomizations(
                  undefined,
                  domainKnowledgeDraft.id,
                  summaryTemplateDraft.id,
                )
              }
            >
              {"\u518d\u8aad\u307f\u8fbc\u307f"}
            </button>
          </div>

          {customizationLoading ? (
            <output className="loading settings-inline-status">
              <span className="loading-spinner" />
              {"\u8aad\u307f\u8fbc\u307f\u4e2d"}
            </output>
          ) : null}

          {customizationError ? (
            <div className="settings-inline-error" role="alert">
              {customizationError}
            </div>
          ) : null}

          <div className="settings-customization-grid">
            <form
              className="settings-customization-panel"
              onSubmit={handleDomainKnowledgeSave}
            >
              <div className="settings-customization-header">
                <div>
                  <h3>{"\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58"}</h3>
                  <p>
                    {activeDomainKnowledgeItems.length > 0
                      ? `\u6709\u52b9: ${activeDomainKnowledgeItems
                          .map((item) => `${item.title} v${item.version}`)
                          .join(", ")}`
                      : "\u6709\u52b9\u306a\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u306f\u3042\u308a\u307e\u305b\u3093"}
                  </p>
                </div>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={customizationControlsDisabled}
                  onClick={() => handleDomainKnowledgeSelect("")}
                >
                  {"\u65b0\u898f"}
                </button>
              </div>

              <label className="settings-field">
                <span>{"\u30d0\u30fc\u30b8\u30e7\u30f3"}</span>
                <select
                  value={domainKnowledgeDraft.id ?? ""}
                  disabled={
                    customizationControlsDisabled ||
                    domainKnowledgeItems.length === 0
                  }
                  onChange={(event) =>
                    handleDomainKnowledgeSelect(event.target.value)
                  }
                >
                  <option value="">
                    {"\u65b0\u898f\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58"}
                  </option>
                  {domainKnowledgeItems.map((item) => (
                    <option key={item.id} value={item.id}>
                      {`${item.title} v${item.version} / ${
                        domainKnowledgeTypeLabels[item.content_type]
                      }${
                        item.archived_at
                          ? " / \u30a2\u30fc\u30ab\u30a4\u30d6\u6e08\u307f"
                          : item.active
                            ? " / \u6709\u52b9"
                            : " / \u4e0b\u66f8\u304d"
                      }`}
                    </option>
                  ))}
                </select>
              </label>

              <label className="settings-field">
                <span>{"\u7a2e\u5225"}</span>
                <select
                  value={domainKnowledgeDraft.content_type}
                  disabled={
                    customizationControlsDisabled ||
                    selectedDomainKnowledgeItem?.archived_at != null
                  }
                  onChange={(event) =>
                    updateDomainKnowledgeDraft({
                      content_type: event.target
                        .value as DomainKnowledgeContentType,
                    })
                  }
                >
                  {domainKnowledgeContentTypes.map((contentType) => (
                    <option key={contentType} value={contentType}>
                      {domainKnowledgeTypeLabels[contentType]}
                    </option>
                  ))}
                </select>
              </label>

              <label className="settings-field">
                <span>{"\u30bf\u30a4\u30c8\u30eb"}</span>
                <input
                  type="text"
                  value={domainKnowledgeDraft.title}
                  disabled={
                    customizationControlsDisabled ||
                    selectedDomainKnowledgeItem?.archived_at != null
                  }
                  onChange={(event) =>
                    updateDomainKnowledgeDraft({ title: event.target.value })
                  }
                />
              </label>

              <label className="settings-field">
                <span>{"\u672c\u6587"}</span>
                <textarea
                  rows={7}
                  value={domainKnowledgeDraft.body}
                  disabled={
                    customizationControlsDisabled ||
                    selectedDomainKnowledgeItem?.archived_at != null
                  }
                  onChange={(event) =>
                    updateDomainKnowledgeDraft({ body: event.target.value })
                  }
                />
              </label>

              <label className="settings-checkbox">
                <input
                  type="checkbox"
                  checked={domainKnowledgeDraft.active}
                  disabled={
                    customizationControlsDisabled ||
                    selectedDomainKnowledgeItem?.archived_at != null
                  }
                  onChange={(event) =>
                    updateDomainKnowledgeDraft({
                      active: event.target.checked,
                    })
                  }
                />
                <span>
                  {"\u4fdd\u5b58\u6642\u306b\u6709\u52b9\u306b\u3059\u308b"}
                </span>
              </label>

              {selectedDomainKnowledgeItem ? (
                <p className="settings-version-meta">
                  {`v${selectedDomainKnowledgeItem.version} / ${
                    selectedDomainKnowledgeItem.archived_at
                      ? "\u30a2\u30fc\u30ab\u30a4\u30d6\u6e08\u307f"
                      : selectedDomainKnowledgeItem.active
                        ? "\u6709\u52b9"
                        : "\u4e0b\u66f8\u304d"
                  } / \u66f4\u65b0: ${selectedDomainKnowledgeItem.updated_at}`}
                </p>
              ) : null}

              <div className="settings-token-actions">
                <button
                  type="submit"
                  className="primary-button"
                  disabled={
                    customizationControlsDisabled ||
                    selectedDomainKnowledgeItem?.archived_at != null
                  }
                >
                  {activeOperation === "domain-save"
                    ? "\u4fdd\u5b58\u4e2d"
                    : "\u30c9\u30e1\u30a4\u30f3\u77e5\u8b58\u3092\u4fdd\u5b58"}
                </button>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={
                    customizationControlsDisabled ||
                    !selectedDomainKnowledgeItem ||
                    selectedDomainKnowledgeItem.active ||
                    selectedDomainKnowledgeItem.archived_at != null
                  }
                  onClick={handleDomainKnowledgeActivate}
                >
                  {activeOperation === "domain-activate"
                    ? "\u6709\u52b9\u5316\u4e2d"
                    : "\u6709\u52b9\u5316"}
                </button>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={
                    customizationControlsDisabled ||
                    !selectedDomainKnowledgeItem ||
                    selectedDomainKnowledgeItem.archived_at != null
                  }
                  onClick={handleDomainKnowledgeArchive}
                >
                  {activeOperation === "domain-archive"
                    ? "\u30a2\u30fc\u30ab\u30a4\u30d6\u4e2d"
                    : "\u30a2\u30fc\u30ab\u30a4\u30d6"}
                </button>
              </div>
            </form>

            <form
              className="settings-customization-panel"
              onSubmit={handleSummaryTemplateSave}
            >
              <div className="settings-customization-header">
                <div>
                  <h3>{"\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8"}</h3>
                  <p>
                    {activeSummaryTemplate
                      ? `\u6709\u52b9: ${activeSummaryTemplate.name} v${activeSummaryTemplate.version}`
                      : "\u6709\u52b9\u306a\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u306f\u3042\u308a\u307e\u305b\u3093"}
                  </p>
                </div>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={customizationControlsDisabled}
                  onClick={() => handleSummaryTemplateSelect("")}
                >
                  {"\u65b0\u898f"}
                </button>
              </div>

              <label className="settings-field">
                <span>{"\u30d0\u30fc\u30b8\u30e7\u30f3"}</span>
                <select
                  value={summaryTemplateDraft.id ?? ""}
                  disabled={
                    customizationControlsDisabled ||
                    summaryTemplates.length === 0
                  }
                  onChange={(event) =>
                    handleSummaryTemplateSelect(event.target.value)
                  }
                >
                  <option value="">
                    {
                      "\u65b0\u898f\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8"
                    }
                  </option>
                  {summaryTemplates.map((item) => (
                    <option key={item.id} value={item.id}>
                      {`${item.name} v${item.version}${
                        item.archived_at
                          ? " / \u30a2\u30fc\u30ab\u30a4\u30d6\u6e08\u307f"
                          : item.active
                            ? " / \u6709\u52b9"
                            : " / \u4e0b\u66f8\u304d"
                      }`}
                    </option>
                  ))}
                </select>
              </label>

              <label className="settings-field">
                <span>{"\u540d\u524d"}</span>
                <input
                  type="text"
                  value={summaryTemplateDraft.name}
                  disabled={
                    customizationControlsDisabled ||
                    selectedSummaryTemplate?.archived_at != null
                  }
                  onChange={(event) =>
                    updateSummaryTemplateDraft({ name: event.target.value })
                  }
                />
              </label>

              <label className="settings-field">
                <span>{"\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8"}</span>
                <textarea
                  rows={9}
                  value={summaryTemplateDraft.template}
                  disabled={
                    customizationControlsDisabled ||
                    selectedSummaryTemplate?.archived_at != null
                  }
                  onChange={(event) =>
                    updateSummaryTemplateDraft({
                      template: event.target.value,
                    })
                  }
                />
              </label>

              <label className="settings-checkbox">
                <input
                  type="checkbox"
                  checked={summaryTemplateDraft.active}
                  disabled={
                    customizationControlsDisabled ||
                    selectedSummaryTemplate?.archived_at != null
                  }
                  onChange={(event) =>
                    updateSummaryTemplateDraft({
                      active: event.target.checked,
                    })
                  }
                />
                <span>
                  {"\u4fdd\u5b58\u6642\u306b\u6709\u52b9\u306b\u3059\u308b"}
                </span>
              </label>

              {selectedSummaryTemplate ? (
                <p className="settings-version-meta">
                  {`v${selectedSummaryTemplate.version} / ${
                    selectedSummaryTemplate.archived_at
                      ? "\u30a2\u30fc\u30ab\u30a4\u30d6\u6e08\u307f"
                      : selectedSummaryTemplate.active
                        ? "\u6709\u52b9"
                        : "\u4e0b\u66f8\u304d"
                  } / \u66f4\u65b0: ${selectedSummaryTemplate.updated_at}`}
                </p>
              ) : null}

              <div className="settings-token-actions">
                <button
                  type="submit"
                  className="primary-button"
                  disabled={
                    customizationControlsDisabled ||
                    selectedSummaryTemplate?.archived_at != null
                  }
                >
                  {activeOperation === "template-save"
                    ? "\u4fdd\u5b58\u4e2d"
                    : "\u8981\u7d04\u30c6\u30f3\u30d7\u30ec\u30fc\u30c8\u3092\u4fdd\u5b58"}
                </button>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={
                    customizationControlsDisabled ||
                    !selectedSummaryTemplate ||
                    selectedSummaryTemplate.active ||
                    selectedSummaryTemplate.archived_at != null
                  }
                  onClick={handleSummaryTemplateActivate}
                >
                  {activeOperation === "template-activate"
                    ? "\u6709\u52b9\u5316\u4e2d"
                    : "\u6709\u52b9\u5316"}
                </button>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={
                    customizationControlsDisabled ||
                    !selectedSummaryTemplate ||
                    selectedSummaryTemplate.archived_at != null
                  }
                  onClick={handleSummaryTemplateArchive}
                >
                  {activeOperation === "template-archive"
                    ? "\u30a2\u30fc\u30ab\u30a4\u30d6\u4e2d"
                    : "\u30a2\u30fc\u30ab\u30a4\u30d6"}
                </button>
              </div>
            </form>
          </div>
        </section>
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
