import {
  type FormEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { ForbiddenState } from "../components/ForbiddenState";
import {
  activateDomainKnowledgeItem,
  activateSummaryTemplate,
  archiveAiMemoryNote,
  archiveDomainKnowledgeItem,
  archivePersonAlias,
  archiveSummaryTemplate,
  createAiMemoryNote,
  createDomainKnowledgeItem,
  createPersonAlias,
  createSummaryTemplate,
  deleteGuildBotToken,
  fetchAdminRetentionOverview,
  fetchAiMemoryNotes,
  fetchDomainKnowledgeItems,
  fetchGuildSettings,
  fetchPersonAliases,
  fetchSummaryTemplates,
  fetchTranscriptFeedbackQueue,
  pinAiMemoryNote,
  previewAdminRetentionCleanup,
  previewAdminRetentionMeetingDelete,
  promoteAiMemoryToDomainKnowledge,
  runAdminRetentionCleanup,
  runAdminRetentionMeetingDelete,
  unpinAiMemoryNote,
  updateAiMemoryNote,
  updateDomainKnowledgeItem,
  updateGuildBotToken,
  updateGuildSettings,
  updatePersonAlias,
  updateSummaryTemplate,
  updateTranscriptFeedbackStatus,
} from "../lib/api";
import type {
  AdminRetentionCleanupPreview,
  AdminRetentionCleanupRun,
  AdminRetentionMeetingDelete,
  AdminRetentionMeetingDeletePreview,
  AdminRetentionOverview,
  AdminRetentionPolicyRequest,
  AdminRetentionTargets,
  AiMemoryNote,
  AiMemoryTag,
  AiMemoryUpsertRequest,
  DomainKnowledgeContentType,
  DomainKnowledgeItem,
  DomainKnowledgeUpsertRequest,
  GuildSettingsResponse,
  PersonAlias,
  PersonAliasReviewStatus,
  PersonAliasUpsertRequest,
  SummaryTemplate,
  SummaryTemplateUpsertRequest,
  TranscriptFeedbackResponse,
  TranscriptFeedbackStatusRequest,
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
  | "memory-save"
  | "memory-pin"
  | "memory-archive"
  | "memory-promote"
  | "feedback-status"
  | "alias-save"
  | "alias-archive"
  | "template-save"
  | "template-activate"
  | "template-archive"
  | "retention-load"
  | "retention-preview"
  | "retention-run"
  | "retention-meeting-preview"
  | "retention-meeting-delete";

interface RetentionAdminDraft {
  token: string;
  summary_ttl_days: string;
  meeting_id: string;
  reason: string;
  targets: AdminRetentionTargets;
}

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

interface AiMemoryDraft {
  id: string | null;
  title: string;
  body: string;
  tagsText: string;
  confidence: string;
  active: boolean;
  pinned: boolean;
  promoteContentType: DomainKnowledgeContentType;
}

interface PersonAliasDraft {
  id: string | null;
  canonical_name: string;
  alias: string;
  discord_user_id: string;
  confidence: string;
  active: boolean;
  review_status: PersonAliasReviewStatus;
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

const aiMemoryTags: AiMemoryTag[] = [
  "person",
  "alias",
  "project",
  "product",
  "terminology",
  "decision",
  "team_convention",
  "summary_hint",
  "transcription_hint",
  "uncertain",
];

const aiMemoryTagLabels: Record<AiMemoryTag, string> = {
  person: "人",
  alias: "別名",
  project: "プロジェクト",
  product: "プロダクト",
  terminology: "用語",
  decision: "決定事項",
  team_convention: "チーム慣習",
  summary_hint: "要約ヒント",
  transcription_hint: "文字起こしヒント",
  uncertain: "要確認",
};

const aiMemorySourceLabels: Record<string, string> = {
  ai_meeting_extraction: "AI抽出",
  user_feedback: "フィードバック",
  manual: "手動",
  vc_participant: "VC参加者",
  promotion_candidate: "昇格候補",
};

const feedbackTypeLabels: Record<string, string> = {
  mistranscription: "文字起こし",
  speaker: "話者",
  term: "用語",
  person_alias: "人名別名",
  domain_knowledge: "ドメイン知識",
  ai_memory: "AIメモ",
};

const feedbackStatusLabels: Record<string, string> = {
  open: "未対応",
  accepted: "採用",
  dismissed: "却下",
  converted_to_domain_knowledge: "ドメイン知識化",
  converted_to_ai_memory: "AIメモ化",
};

const personAliasReviewStatusLabels: Record<PersonAliasReviewStatus, string> = {
  unreviewed: "未確認",
  accepted: "採用",
  dismissed: "却下",
};

const personAliasSourceLabels: Record<string, string> = {
  user_feedback: "フィードバック",
  ai_inference: "AI推定",
  vc_participant: "VC参加者",
  manual: "手動",
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

function emptyAiMemoryDraft(): AiMemoryDraft {
  return {
    id: null,
    title: "",
    body: "",
    tagsText: "",
    confidence: "",
    active: true,
    pinned: false,
    promoteContentType: "glossary",
  };
}

function emptyPersonAliasDraft(): PersonAliasDraft {
  return {
    id: null,
    canonical_name: "",
    alias: "",
    discord_user_id: "",
    confidence: "",
    active: true,
    review_status: "unreviewed",
  };
}

function emptyRetentionAdminDraft(): RetentionAdminDraft {
  return {
    token: "",
    summary_ttl_days: "",
    meeting_id: "",
    reason: "",
    targets: {
      raw_audio: true,
      transcript: true,
      summary: true,
      debug: true,
    },
  };
}

function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB", "TB"];
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function retentionPolicyRequestFromDraft(
  draft: RetentionAdminDraft,
  form: SettingsForm | null,
): AdminRetentionPolicyRequest {
  const summaryTtl = draft.summary_ttl_days.trim();
  return {
    raw_audio_ttl_days: form
      ? readNumber(form.retention_raw_audio_ttl_days)
      : undefined,
    transcript_ttl_days: form
      ? readNumber(form.retention_transcript_ttl_days)
      : undefined,
    summary_ttl_days: summaryTtl === "" ? undefined : readNumber(summaryTtl),
  };
}

function validateRetentionPolicyDraft(
  draft: RetentionAdminDraft,
  form: SettingsForm | null,
): string | null {
  if (!form) {
    return "設定を読み込んでから保持条件を確認してください";
  }
  const rawAudioTtlDays = readNumber(form.retention_raw_audio_ttl_days);
  const transcriptTtlDays = readNumber(form.retention_transcript_ttl_days);
  const summaryTtl = draft.summary_ttl_days.trim();
  const summaryTtlDays = summaryTtl === "" ? null : readNumber(summaryTtl);
  if (
    !Number.isFinite(rawAudioTtlDays) ||
    !Number.isFinite(transcriptTtlDays) ||
    rawAudioTtlDays < 1 ||
    rawAudioTtlDays > 365 ||
    transcriptTtlDays < 1 ||
    transcriptTtlDays > 365 ||
    (summaryTtlDays !== null &&
      (!Number.isFinite(summaryTtlDays) ||
        summaryTtlDays < 1 ||
        summaryTtlDays > 365))
  ) {
    return "保持日数は1から365の整数で入力してください";
  }
  return null;
}

function retentionPolicyKey(request: AdminRetentionPolicyRequest): string {
  return [
    request.raw_audio_ttl_days ?? "",
    request.transcript_ttl_days ?? "",
    request.summary_ttl_days ?? "",
  ].join("|");
}

function retentionMeetingDeleteRequest(draft: RetentionAdminDraft) {
  const reason = draft.reason.trim();
  return {
    targets: draft.targets,
    reason: reason === "" ? null : reason,
  };
}

function retentionMeetingPreviewKeyFor(
  meetingId: string,
  targets: AdminRetentionTargets,
): string {
  return [
    meetingId,
    targets.raw_audio ? "raw" : "",
    targets.transcript ? "transcript" : "",
    targets.summary ? "summary" : "",
    targets.debug ? "debug" : "",
  ].join("|");
}

function sameRetentionTargets(
  left: AdminRetentionTargets,
  right: AdminRetentionTargets,
): boolean {
  return (
    left.raw_audio === right.raw_audio &&
    left.transcript === right.transcript &&
    left.summary === right.summary &&
    left.debug === right.debug
  );
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

function aiMemoryDraftFromItem(item: AiMemoryNote): AiMemoryDraft {
  return {
    id: item.id,
    title: item.title,
    body: item.body,
    tagsText: item.tags.join(", "),
    confidence: item.confidence == null ? "" : String(item.confidence),
    active: item.active,
    pinned: item.pinned,
    promoteContentType: "glossary",
  };
}

function personAliasDraftFromItem(item: PersonAlias): PersonAliasDraft {
  return {
    id: item.id,
    canonical_name: item.canonical_name,
    alias: item.alias,
    discord_user_id: item.discord_user_id ?? "",
    confidence: item.confidence == null ? "" : String(item.confidence),
    active: item.active,
    review_status: item.review_status,
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

function chooseAiMemoryDraft(
  items: AiMemoryNote[],
  preferredId?: string | null,
): AiMemoryDraft {
  const selected =
    (preferredId ? items.find((item) => item.id === preferredId) : null) ??
    items.find((item) => item.pinned && item.archived_at == null) ??
    items.find((item) => item.active && item.archived_at == null) ??
    items.find((item) => item.archived_at == null) ??
    items[0];
  return selected ? aiMemoryDraftFromItem(selected) : emptyAiMemoryDraft();
}

function choosePersonAliasDraft(
  items: PersonAlias[],
  preferredId?: string | null,
): PersonAliasDraft {
  const selected =
    (preferredId ? items.find((item) => item.id === preferredId) : null) ??
    items.find(
      (item) => item.review_status === "unreviewed" && item.archived_at == null,
    ) ??
    items.find((item) => item.active && item.archived_at == null) ??
    items.find((item) => item.archived_at == null) ??
    items[0];
  return selected
    ? personAliasDraftFromItem(selected)
    : emptyPersonAliasDraft();
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

function aiMemoryRequestFromDraft(draft: AiMemoryDraft): AiMemoryUpsertRequest {
  const confidence = draft.confidence.trim();
  return {
    title: draft.title.trim(),
    body: draft.body.trim(),
    tags: parseAiMemoryTags(draft.tagsText),
    confidence: confidence === "" ? null : Number(confidence),
    active: draft.active,
    pinned: draft.pinned,
  };
}

function personAliasRequestFromDraft(
  draft: PersonAliasDraft,
): PersonAliasUpsertRequest {
  const discordUserId = draft.discord_user_id.trim();
  const confidence = draft.confidence.trim();
  return {
    canonical_name: draft.canonical_name.trim(),
    alias: draft.alias.trim(),
    discord_user_id: discordUserId === "" ? null : discordUserId,
    confidence: confidence === "" ? null : Number(confidence),
    active: draft.active,
    review_status: draft.review_status,
  };
}

function parseAiMemoryTags(value: string): AiMemoryTag[] {
  const tags = value
    .split(",")
    .map((tag) => tag.trim())
    .filter(Boolean);
  return Array.from(new Set(tags)) as AiMemoryTag[];
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

function validateAiMemoryDraft(draft: AiMemoryDraft): string | null {
  const title = draft.title.trim();
  const body = draft.body.trim();
  if (!title) {
    return "AIメモのタイトルを入力してください";
  }
  if (utf8ByteLength(title) > 200) {
    return "AIメモのタイトルはUTF-8で200バイト以内で入力してください";
  }
  if (!body) {
    return "AIメモの本文を入力してください";
  }
  if (utf8ByteLength(body) > 20_000) {
    return "AIメモの本文はUTF-8で20000バイト以内で入力してください";
  }
  const tags = draft.tagsText
    .split(",")
    .map((tag) => tag.trim())
    .filter(Boolean);
  if (tags.length > 10) {
    return "AIメモのタグは10個以内で入力してください";
  }
  const invalidTag = tags.find(
    (tag): tag is string => !aiMemoryTags.includes(tag as AiMemoryTag),
  );
  if (invalidTag) {
    return `使用できないAIメモタグです: ${invalidTag}`;
  }
  const confidence = draft.confidence.trim();
  if (confidence !== "") {
    const parsed = Number(confidence);
    if (!Number.isFinite(parsed) || parsed < 0 || parsed > 1) {
      return "AIメモの信頼度は0から1で入力してください";
    }
  }
  return null;
}

function validatePersonAliasDraft(draft: PersonAliasDraft): string | null {
  const canonicalName = draft.canonical_name.trim();
  const alias = draft.alias.trim();
  if (!canonicalName) {
    return "正式名を入力してください";
  }
  if (utf8ByteLength(canonicalName) > 200) {
    return "正式名はUTF-8で200バイト以内で入力してください";
  }
  if (!alias) {
    return "別名を入力してください";
  }
  if (utf8ByteLength(alias) > 200) {
    return "別名はUTF-8で200バイト以内で入力してください";
  }
  if (utf8ByteLength(draft.discord_user_id.trim()) > 128) {
    return "DiscordユーザーIDはUTF-8で128バイト以内で入力してください";
  }
  const confidence = draft.confidence.trim();
  if (confidence !== "") {
    const parsed = Number(confidence);
    if (!Number.isFinite(parsed) || parsed < 0 || parsed > 1) {
      return "人名別名の信頼度は0から1で入力してください";
    }
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

function labelFromRecord(
  record: Record<string, string>,
  value: string,
): string {
  return record[value] ?? value;
}

function feedbackText(item: TranscriptFeedbackResponse): string {
  const parts = [
    item.original_text ? `元: ${item.original_text}` : null,
    item.corrected_text ? `修正: ${item.corrected_text}` : null,
    item.note ? `メモ: ${item.note}` : null,
    item.speaker_id ? `話者: ${item.speaker_id}` : null,
    item.corrected_speaker_id ? `修正話者: ${item.corrected_speaker_id}` : null,
    item.term_type ? `用語種別: ${item.term_type}` : null,
  ].filter(Boolean);
  return parts.length > 0 ? parts.join(" / ") : "詳細はありません";
}

function feedbackStatusRequest(
  status: TranscriptFeedbackStatusRequest["status"],
): TranscriptFeedbackStatusRequest {
  return { status };
}

function feedbackActionLabel(
  item: TranscriptFeedbackResponse,
  action: string,
): string {
  return `${
    item.corrected_text ?? item.original_text ?? item.note ?? item.id
  } を${action}`;
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

interface SettingsPageProps {
  guildId?: string;
  guildName?: string;
  showCustomizations?: boolean;
}

export function SettingsPage({
  guildId,
  guildName,
  showCustomizations = true,
}: SettingsPageProps = {}) {
  const currentGuildKeyRef = useRef<string | null>(guildId ?? null);
  currentGuildKeyRef.current = guildId ?? null;
  const [settings, setSettings] = useState<GuildSettingsResponse | null>(null);
  const [form, setForm] = useState<SettingsForm | null>(null);
  const [domainKnowledgeItems, setDomainKnowledgeItems] = useState<
    DomainKnowledgeItem[]
  >([]);
  const [aiMemoryNotes, setAiMemoryNotes] = useState<AiMemoryNote[]>([]);
  const [feedbackItems, setFeedbackItems] = useState<
    TranscriptFeedbackResponse[]
  >([]);
  const [personAliases, setPersonAliases] = useState<PersonAlias[]>([]);
  const [summaryTemplates, setSummaryTemplates] = useState<SummaryTemplate[]>(
    [],
  );
  const [domainKnowledgeDraft, setDomainKnowledgeDraft] =
    useState<DomainKnowledgeDraft>(emptyDomainKnowledgeDraft);
  const [aiMemoryDraft, setAiMemoryDraft] =
    useState<AiMemoryDraft>(emptyAiMemoryDraft);
  const [personAliasDraft, setPersonAliasDraft] = useState<PersonAliasDraft>(
    emptyPersonAliasDraft,
  );
  const [summaryTemplateDraft, setSummaryTemplateDraft] =
    useState<SummaryTemplateDraft>(emptySummaryTemplateDraft);
  const [retentionDraft, setRetentionDraft] = useState<RetentionAdminDraft>(
    emptyRetentionAdminDraft,
  );
  const [retentionOverview, setRetentionOverview] =
    useState<AdminRetentionOverview | null>(null);
  const [retentionCleanupPreview, setRetentionCleanupPreview] =
    useState<AdminRetentionCleanupPreview | null>(null);
  const [retentionCleanupPreviewKey, setRetentionCleanupPreviewKey] = useState<
    string | null
  >(null);
  const [retentionCleanupRun, setRetentionCleanupRun] =
    useState<AdminRetentionCleanupRun | null>(null);
  const [retentionMeetingPreview, setRetentionMeetingPreview] =
    useState<AdminRetentionMeetingDeletePreview | null>(null);
  const [retentionMeetingPreviewKey, setRetentionMeetingPreviewKey] = useState<
    string | null
  >(null);
  const [retentionMeetingDelete, setRetentionMeetingDelete] =
    useState<AdminRetentionMeetingDelete | null>(null);
  const [retentionError, setRetentionError] = useState<string | null>(null);
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
      preferredAiMemoryId?: string | null,
      preferredPersonAliasId?: string | null,
      preferredSummaryTemplateId?: string | null,
    ) => {
      setCustomizationLoading(true);
      setCustomizationError(null);
      const requestGuildKey = currentGuildKeyRef.current;

      try {
        const [domainItems, memoryNotes, feedbackQueue, aliases, templates] =
          await Promise.all([
            fetchDomainKnowledgeItems({ includeArchived: true }, signal),
            fetchAiMemoryNotes({ includeArchived: true }, signal),
            fetchTranscriptFeedbackQueue({ status: "open" }, signal),
            fetchPersonAliases({ includeArchived: true }, signal),
            fetchSummaryTemplates({ includeArchived: true }, signal),
          ]);
        if (signal?.aborted || currentGuildKeyRef.current !== requestGuildKey) {
          return;
        }
        setDomainKnowledgeItems(domainItems);
        setAiMemoryNotes(memoryNotes);
        setFeedbackItems(feedbackQueue);
        setPersonAliases(aliases);
        setSummaryTemplates(templates);
        setDomainKnowledgeDraft((current) =>
          chooseDomainKnowledgeDraft(
            domainItems,
            preferredDomainKnowledgeId === undefined
              ? current.id
              : preferredDomainKnowledgeId,
          ),
        );
        setAiMemoryDraft((current) =>
          chooseAiMemoryDraft(
            memoryNotes,
            preferredAiMemoryId === undefined
              ? current.id
              : preferredAiMemoryId,
          ),
        );
        setPersonAliasDraft((current) =>
          choosePersonAliasDraft(
            aliases,
            preferredPersonAliasId === undefined
              ? current.id
              : preferredPersonAliasId,
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
        if (signal?.aborted || currentGuildKeyRef.current !== requestGuildKey) {
          return;
        }
        setCustomizationError(
          customizationErrorMessage(
            err,
            "AIカスタマイズの読み込みに失敗しました",
          ),
        );
      } finally {
        if (
          !signal?.aborted &&
          currentGuildKeyRef.current === requestGuildKey
        ) {
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
    setActiveOperation(null);
    setSettings(null);
    setForm(null);
    setBotTokenValue("");
    setTokenDeleteConfirmPending(false);
    setRetentionOverview(null);
    setRetentionCleanupPreview(null);
    setRetentionCleanupPreviewKey(null);
    setRetentionCleanupRun(null);
    setRetentionMeetingPreview(null);
    setRetentionMeetingPreviewKey(null);
    setRetentionMeetingDelete(null);
    setRetentionError(null);
    if (!showCustomizations) {
      setDomainKnowledgeItems([]);
      setAiMemoryNotes([]);
      setFeedbackItems([]);
      setPersonAliases([]);
      setSummaryTemplates([]);
      setDomainKnowledgeDraft(emptyDomainKnowledgeDraft());
      setAiMemoryDraft(emptyAiMemoryDraft());
      setPersonAliasDraft(emptyPersonAliasDraft());
      setSummaryTemplateDraft(emptySummaryTemplateDraft());
    }

    fetchGuildSettings(guildId, controller.signal)
      .then((settingsResponse) => {
        if (!controller.signal.aborted) {
          setSettings(settingsResponse);
          setForm(formFromSettings(settingsResponse));
          if (settingsResponse.is_admin && showCustomizations) {
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
  }, [guildId, refreshCustomizations, showCustomizations]);

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
  const selectedAiMemoryNote = aiMemoryDraft.id
    ? (aiMemoryNotes.find((item) => item.id === aiMemoryDraft.id) ?? null)
    : null;
  const selectedPersonAlias = personAliasDraft.id
    ? (personAliases.find((item) => item.id === personAliasDraft.id) ?? null)
    : null;
  const activeDomainKnowledgeItems = domainKnowledgeItems.filter(
    (item) => item.active && item.archived_at == null,
  );
  const activeAiMemoryNotes = aiMemoryNotes.filter(
    (item) => item.active && item.archived_at == null,
  );
  const pinnedAiMemoryNotes = aiMemoryNotes.filter(
    (item) => item.pinned && item.archived_at == null,
  );
  const activePersonAliases = personAliases.filter(
    (item) => item.active && item.archived_at == null,
  );
  const activeSummaryTemplate = summaryTemplates.find(
    (item) => item.active && item.archived_at == null,
  );
  const customizationControlsDisabled =
    !canEdit || customizationLoading || isSavingAny;
  const retentionPolicyDraft = retentionPolicyRequestFromDraft(
    retentionDraft,
    form,
  );
  const retentionPolicyDraftKey = retentionPolicyKey(retentionPolicyDraft);
  const retentionCleanupPreviewMatchesDraft =
    retentionCleanupPreviewKey === retentionPolicyDraftKey;
  const retentionTargetsSelected = Object.values(retentionDraft.targets).some(
    Boolean,
  );
  const retentionMeetingPreviewMatchesDraft =
    retentionMeetingPreviewKey ===
      retentionMeetingPreviewKeyFor(
        retentionDraft.meeting_id.trim(),
        retentionDraft.targets,
      ) &&
    retentionMeetingPreview?.meeting_id === retentionDraft.meeting_id.trim() &&
    sameRetentionTargets(
      retentionMeetingPreview.targets,
      retentionDraft.targets,
    );
  const retentionMeetingPreviewStatusAllowsDelete =
    !retentionMeetingPreview ||
    ["posted", "failed", "aborted"].includes(retentionMeetingPreview.status);

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

  function updateAiMemoryDraft(update: Partial<AiMemoryDraft>) {
    setAiMemoryDraft((current) => ({ ...current, ...update }));
    setCustomizationError(null);
    setMessage(null);
  }

  function updatePersonAliasDraft(update: Partial<PersonAliasDraft>) {
    setPersonAliasDraft((current) => ({ ...current, ...update }));
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

  function handleAiMemorySelect(memoryId: string) {
    const selected = aiMemoryNotes.find((item) => item.id === memoryId);
    setAiMemoryDraft(
      selected ? aiMemoryDraftFromItem(selected) : emptyAiMemoryDraft(),
    );
    setCustomizationError(null);
    setMessage(null);
  }

  function handlePersonAliasSelect(aliasId: string) {
    const selected = personAliases.find((item) => item.id === aliasId);
    setPersonAliasDraft(
      selected ? personAliasDraftFromItem(selected) : emptyPersonAliasDraft(),
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
    const requestGuildKey = guildId ?? null;

    try {
      const updated = await updateGuildSettings(requestFromForm(form), guildId);
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setSettings(updated);
      setForm(formFromSettings(updated));
      setMessage("\u8a2d\u5b9a\u3092\u4fdd\u5b58\u3057\u307e\u3057\u305f");
    } catch (err) {
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      const text =
        err instanceof Error && err.message === "forbidden"
          ? "\u7ba1\u7406\u6a29\u9650\u304c\u306a\u3044\u305f\u3081\u4fdd\u5b58\u3067\u304d\u307e\u305b\u3093"
          : "\u8a2d\u5b9a\u306e\u4fdd\u5b58\u306b\u5931\u6557\u3057\u307e\u3057\u305f";
      setError(text);
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
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
    const requestGuildKey = guildId ?? null;

    try {
      const updated = await updateGuildBotToken({ bot_token: token }, guildId);
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setSettings(updated);
      setBotTokenValue("");
      setMessage(
        "Discord Bot token \u3092\u4fdd\u5b58\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setError(
        guildSettingsErrorMessage(
          err,
          "Discord Bot token \u306e\u4fdd\u5b58\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
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
    const requestGuildKey = guildId ?? null;

    try {
      const updated = await deleteGuildBotToken(guildId);
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setSettings(updated);
      setBotTokenValue("");
      setMessage(
        "Discord Bot token \u306e\u30ae\u30eb\u30c9\u500b\u5225\u8a2d\u5b9a\u3092\u524a\u9664\u3057\u307e\u3057\u305f",
      );
    } catch (err) {
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setError(
        guildSettingsErrorMessage(
          err,
          "Discord Bot token \u306e\u524a\u9664\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        ),
      );
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setTokenDeleteConfirmPending(false);
        setActiveOperation(null);
      }
    }
  }

  function updateRetentionDraft(update: Partial<RetentionAdminDraft>) {
    setRetentionDraft((current) => ({ ...current, ...update }));
    setRetentionError(null);
    setMessage(null);
  }

  function updateRetentionTarget(
    target: keyof AdminRetentionTargets,
    value: boolean,
  ) {
    setRetentionDraft((current) => ({
      ...current,
      targets: { ...current.targets, [target]: value },
    }));
    setRetentionError(null);
    setMessage(null);
  }

  async function handleRetentionLoad(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const token = retentionDraft.token.trim();
    const requestGuildKey = currentGuildKeyRef.current;
    if (!token || isSavingAny) {
      setRetentionError("管理トークンを入力してください");
      return;
    }
    setActiveOperation("retention-load");
    setRetentionError(null);
    try {
      const overview = await fetchAdminRetentionOverview({
        bearerToken: token,
      });
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionOverview(overview);
      setMessage("保持管理情報を読み込みました");
    } catch (err) {
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionError(
        customizationErrorMessage(err, "保持管理情報の読み込みに失敗しました"),
      );
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handleRetentionCleanupPreview() {
    const token = retentionDraft.token.trim();
    const requestGuildKey = currentGuildKeyRef.current;
    if (!token || isSavingAny) {
      setRetentionError("管理トークンを入力してください");
      return;
    }
    const validationError = validateRetentionPolicyDraft(retentionDraft, form);
    if (validationError) {
      setRetentionError(validationError);
      return;
    }
    const request = retentionPolicyRequestFromDraft(retentionDraft, form);
    const requestKey = retentionPolicyKey(request);
    setActiveOperation("retention-preview");
    setRetentionError(null);
    try {
      const preview = await previewAdminRetentionCleanup(request, {
        bearerToken: token,
      });
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionCleanupPreview(preview);
      setRetentionCleanupPreviewKey(requestKey);
      setRetentionCleanupRun(null);
      setMessage("クリーンアップ候補を確認しました");
    } catch (err) {
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionError(
        customizationErrorMessage(
          err,
          "クリーンアップ候補の確認に失敗しました",
        ),
      );
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handleRetentionCleanupRun() {
    const token = retentionDraft.token.trim();
    const requestGuildKey = currentGuildKeyRef.current;
    const validationError = validateRetentionPolicyDraft(retentionDraft, form);
    if (validationError) {
      setRetentionError(validationError);
      return;
    }
    const request = retentionPolicyRequestFromDraft(retentionDraft, form);
    const requestKey = retentionPolicyKey(request);
    if (!token) {
      setRetentionError("管理トークンを入力してください");
      return;
    }
    if (
      !retentionCleanupPreview ||
      retentionCleanupPreviewKey !== requestKey ||
      isSavingAny
    ) {
      setRetentionError("実行前に現在の保持条件で確認を実行してください");
      return;
    }
    setActiveOperation("retention-run");
    setRetentionError(null);
    try {
      const result = await runAdminRetentionCleanup(request, {
        bearerToken: token,
      });
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionCleanupPreview(result.preview);
      setRetentionCleanupPreviewKey(null);
      setRetentionCleanupRun(result);
      let refreshWarning = false;
      try {
        const overview = await fetchAdminRetentionOverview({
          bearerToken: token,
        });
        if (currentGuildKeyRef.current !== requestGuildKey) {
          return;
        }
        setRetentionOverview(overview);
      } catch {
        if (currentGuildKeyRef.current !== requestGuildKey) {
          return;
        }
        refreshWarning = true;
      }
      setMessage(
        result.error
          ? "一部エラー付きでクリーンアップを実行しました"
          : refreshWarning
            ? "クリーンアップを実行しました。使用量の再読み込みに失敗しました"
            : "クリーンアップを実行しました",
      );
    } catch (err) {
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionError(
        customizationErrorMessage(err, "クリーンアップの実行に失敗しました"),
      );
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handleRetentionMeetingPreview() {
    const token = retentionDraft.token.trim();
    const meetingId = retentionDraft.meeting_id.trim();
    const requestGuildKey = currentGuildKeyRef.current;
    if (!token || !meetingId || !retentionTargetsSelected || isSavingAny) {
      setRetentionError("管理トークンと会議IDを入力してください");
      return;
    }
    setActiveOperation("retention-meeting-preview");
    setRetentionError(null);
    try {
      const preview = await previewAdminRetentionMeetingDelete(
        meetingId,
        retentionMeetingDeleteRequest(retentionDraft),
        { bearerToken: token },
      );
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionMeetingPreview(preview);
      setRetentionMeetingPreviewKey(
        retentionMeetingPreviewKeyFor(meetingId, retentionDraft.targets),
      );
      setRetentionMeetingDelete(null);
      setMessage("会議削除の対象を確認しました");
    } catch (err) {
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionError(
        customizationErrorMessage(err, "会議削除の確認に失敗しました"),
      );
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handleRetentionMeetingDelete() {
    const token = retentionDraft.token.trim();
    const meetingId = retentionDraft.meeting_id.trim();
    const requestGuildKey = currentGuildKeyRef.current;
    if (!token) {
      setRetentionError("管理トークンを入力してください");
      return;
    }
    if (
      !meetingId ||
      !retentionTargetsSelected ||
      !retentionMeetingPreviewMatchesDraft ||
      isSavingAny
    ) {
      setRetentionError("削除前に現在の対象で確認を実行してください");
      return;
    }
    setActiveOperation("retention-meeting-delete");
    setRetentionError(null);
    try {
      const result = await runAdminRetentionMeetingDelete(
        meetingId,
        retentionMeetingDeleteRequest(retentionDraft),
        { bearerToken: token },
      );
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionMeetingPreview(result.preview);
      setRetentionMeetingPreviewKey(null);
      setRetentionMeetingDelete(result);
      let refreshWarning = false;
      try {
        const overview = await fetchAdminRetentionOverview({
          bearerToken: token,
        });
        if (currentGuildKeyRef.current !== requestGuildKey) {
          return;
        }
        setRetentionOverview(overview);
      } catch {
        if (currentGuildKeyRef.current !== requestGuildKey) {
          return;
        }
        refreshWarning = true;
      }
      setMessage(
        result.error
          ? "一部エラー付きで会議コンテンツを削除しました"
          : refreshWarning
            ? "会議コンテンツを削除しました。使用量の再読み込みに失敗しました"
            : "会議コンテンツを削除しました",
      );
    } catch (err) {
      if (currentGuildKeyRef.current !== requestGuildKey) {
        return;
      }
      setRetentionError(
        customizationErrorMessage(err, "会議コンテンツの削除に失敗しました"),
      );
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
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
        aiMemoryDraft.id,
        personAliasDraft.id,
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
        aiMemoryDraft.id,
        personAliasDraft.id,
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
        aiMemoryDraft.id,
        personAliasDraft.id,
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
        aiMemoryDraft.id,
        personAliasDraft.id,
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
        aiMemoryDraft.id,
        personAliasDraft.id,
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
        aiMemoryDraft.id,
        personAliasDraft.id,
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

  async function handleAiMemorySave(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!canEdit || customizationControlsDisabled) {
      return;
    }
    const validationError = validateAiMemoryDraft(aiMemoryDraft);
    if (validationError) {
      setCustomizationError(validationError);
      setMessage(null);
      return;
    }
    if (selectedAiMemoryNote?.archived_at) {
      setCustomizationError("アーカイブ済みのAIメモは保存できません");
      setMessage(null);
      return;
    }

    setActiveOperation("memory-save");
    setCustomizationError(null);
    setMessage(null);
    const requestGuildKey = currentGuildKeyRef.current;

    try {
      const request = aiMemoryRequestFromDraft(aiMemoryDraft);
      const updated = aiMemoryDraft.id
        ? await updateAiMemoryNote(aiMemoryDraft.id, request)
        : await createAiMemoryNote(request);
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        updated.id,
        personAliasDraft.id,
        summaryTemplateDraft.id,
      );
      if (currentGuildKeyRef.current === requestGuildKey) {
        setMessage("AIメモを保存しました");
      }
    } catch (err) {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setCustomizationError(
          customizationErrorMessage(err, "AIメモの保存に失敗しました"),
        );
      }
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handleAiMemoryPinToggle() {
    if (!canEdit || customizationControlsDisabled || !selectedAiMemoryNote) {
      return;
    }

    setActiveOperation("memory-pin");
    setCustomizationError(null);
    setMessage(null);
    const requestGuildKey = currentGuildKeyRef.current;

    try {
      const updated = selectedAiMemoryNote.pinned
        ? await unpinAiMemoryNote(selectedAiMemoryNote.id)
        : await pinAiMemoryNote(selectedAiMemoryNote.id);
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        updated.id,
        personAliasDraft.id,
        summaryTemplateDraft.id,
      );
      if (currentGuildKeyRef.current === requestGuildKey) {
        setMessage(
          updated.pinned
            ? "AIメモをピン留めしました"
            : "AIメモのピン留めを解除しました",
        );
      }
    } catch (err) {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setCustomizationError(
          customizationErrorMessage(err, "AIメモのピン操作に失敗しました"),
        );
      }
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handleAiMemoryArchive() {
    if (
      !canEdit ||
      customizationControlsDisabled ||
      !selectedAiMemoryNote ||
      selectedAiMemoryNote.archived_at
    ) {
      return;
    }

    setActiveOperation("memory-archive");
    setCustomizationError(null);
    setMessage(null);
    const requestGuildKey = currentGuildKeyRef.current;

    try {
      const updated = await archiveAiMemoryNote(selectedAiMemoryNote.id);
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        updated.id,
        personAliasDraft.id,
        summaryTemplateDraft.id,
      );
      if (currentGuildKeyRef.current === requestGuildKey) {
        setMessage("AIメモをアーカイブしました");
      }
    } catch (err) {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setCustomizationError(
          customizationErrorMessage(err, "AIメモのアーカイブに失敗しました"),
        );
      }
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handleAiMemoryPromote() {
    if (
      !canEdit ||
      customizationControlsDisabled ||
      !selectedAiMemoryNote ||
      selectedAiMemoryNote.archived_at
    ) {
      return;
    }

    setActiveOperation("memory-promote");
    setCustomizationError(null);
    setMessage(null);
    const requestGuildKey = currentGuildKeyRef.current;

    try {
      const promoted = await promoteAiMemoryToDomainKnowledge(
        selectedAiMemoryNote.id,
        { content_type: aiMemoryDraft.promoteContentType },
      );
      await refreshCustomizations(
        undefined,
        promoted.id,
        selectedAiMemoryNote.id,
        personAliasDraft.id,
        summaryTemplateDraft.id,
      );
      if (currentGuildKeyRef.current === requestGuildKey) {
        setMessage("AIメモをドメイン知識へ昇格しました");
      }
    } catch (err) {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setCustomizationError(
          customizationErrorMessage(
            err,
            "AIメモのドメイン知識化に失敗しました",
          ),
        );
      }
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handleFeedbackStatus(
    item: TranscriptFeedbackResponse,
    status: TranscriptFeedbackStatusRequest["status"],
  ) {
    if (!canEdit || customizationControlsDisabled) {
      return;
    }

    setActiveOperation("feedback-status");
    setCustomizationError(null);
    setMessage(null);
    const requestGuildKey = currentGuildKeyRef.current;

    try {
      await updateTranscriptFeedbackStatus(
        item.id,
        feedbackStatusRequest(status),
      );
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        aiMemoryDraft.id,
        personAliasDraft.id,
        summaryTemplateDraft.id,
      );
      if (currentGuildKeyRef.current === requestGuildKey) {
        setMessage(
          status === "accepted"
            ? "フィードバックを採用しました"
            : "フィードバックを却下しました",
        );
      }
    } catch (err) {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setCustomizationError(
          customizationErrorMessage(
            err,
            "フィードバックのステータス更新に失敗しました",
          ),
        );
      }
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handlePersonAliasSave(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!canEdit || customizationControlsDisabled) {
      return;
    }
    const validationError = validatePersonAliasDraft(personAliasDraft);
    if (validationError) {
      setCustomizationError(validationError);
      setMessage(null);
      return;
    }
    if (selectedPersonAlias?.archived_at) {
      setCustomizationError("アーカイブ済みの人名別名は保存できません");
      setMessage(null);
      return;
    }

    setActiveOperation("alias-save");
    setCustomizationError(null);
    setMessage(null);
    const requestGuildKey = currentGuildKeyRef.current;

    try {
      const request = personAliasRequestFromDraft(personAliasDraft);
      const updated = personAliasDraft.id
        ? await updatePersonAlias(personAliasDraft.id, request)
        : await createPersonAlias(request);
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        aiMemoryDraft.id,
        updated.id,
        summaryTemplateDraft.id,
      );
      if (currentGuildKeyRef.current === requestGuildKey) {
        setMessage("人名別名を保存しました");
      }
    } catch (err) {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setCustomizationError(
          customizationErrorMessage(err, "人名別名の保存に失敗しました"),
        );
      }
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  async function handlePersonAliasArchive() {
    if (
      !canEdit ||
      customizationControlsDisabled ||
      !selectedPersonAlias ||
      selectedPersonAlias.archived_at
    ) {
      return;
    }

    setActiveOperation("alias-archive");
    setCustomizationError(null);
    setMessage(null);
    const requestGuildKey = currentGuildKeyRef.current;

    try {
      const updated = await archivePersonAlias(selectedPersonAlias.id);
      await refreshCustomizations(
        undefined,
        domainKnowledgeDraft.id,
        aiMemoryDraft.id,
        updated.id,
        summaryTemplateDraft.id,
      );
      if (currentGuildKeyRef.current === requestGuildKey) {
        setMessage("人名別名をアーカイブしました");
      }
    } catch (err) {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setCustomizationError(
          customizationErrorMessage(err, "人名別名のアーカイブに失敗しました"),
        );
      }
    } finally {
      if (currentGuildKeyRef.current === requestGuildKey) {
        setActiveOperation(null);
      }
    }
  }

  if (!loading && forbidden) {
    return (
      <main className="settings-page">
        <div className="settings-header">
          <div>
            <h1>
              {guildName
                ? `${guildName} \u306e\u30ae\u30eb\u30c9\u8a2d\u5b9a`
                : "\u30ae\u30eb\u30c9\u8a2d\u5b9a"}
            </h1>
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
          <h1>
            {guildName
              ? `${guildName} \u306e\u30ae\u30eb\u30c9\u8a2d\u5b9a`
              : "\u30ae\u30eb\u30c9\u8a2d\u5b9a"}
          </h1>
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

      {settings && canEdit ? (
        <section className="settings-section">
          <div className="settings-section-heading">
            <div>
              <h2>{"保持管理"}</h2>
              <p>
                {
                  "Betaでは容量制限は監視のみです。現在の使用量、削除候補、監査付きの手動削除を確認できます"
                }
              </p>
            </div>
          </div>

          <form className="settings-token-form" onSubmit={handleRetentionLoad}>
            <label className="settings-field">
              <span>{"管理トークン"}</span>
              <input
                type="password"
                autoComplete="new-password"
                value={retentionDraft.token}
                disabled={isSavingAny}
                onChange={(event) =>
                  updateRetentionDraft({ token: event.target.value })
                }
              />
            </label>
            <label className="settings-field">
              <span>{"要約保持日数"}</span>
              <input
                type="number"
                min={1}
                max={365}
                step={1}
                placeholder="未設定"
                value={retentionDraft.summary_ttl_days}
                disabled={isSavingAny}
                onChange={(event) =>
                  updateRetentionDraft({
                    summary_ttl_days: event.target.value,
                  })
                }
              />
            </label>
            <div className="settings-token-actions">
              <button
                type="submit"
                className="secondary-button"
                disabled={isSavingAny || retentionDraft.token.trim() === ""}
              >
                {activeOperation === "retention-load"
                  ? "読み込み中"
                  : "使用量を読み込み"}
              </button>
              <button
                type="button"
                className="secondary-button"
                disabled={isSavingAny || retentionDraft.token.trim() === ""}
                onClick={handleRetentionCleanupPreview}
              >
                {activeOperation === "retention-preview"
                  ? "確認中"
                  : "クリーンアップ確認"}
              </button>
              <button
                type="button"
                className="primary-button"
                disabled={
                  isSavingAny ||
                  retentionDraft.token.trim() === "" ||
                  !retentionCleanupPreviewMatchesDraft
                }
                onClick={handleRetentionCleanupRun}
              >
                {activeOperation === "retention-run"
                  ? "実行中"
                  : "クリーンアップ実行"}
              </button>
            </div>
          </form>

          {retentionOverview ? (
            <div className="settings-token-status-row">
              <span className="settings-token-status is-set">
                {formatBytes(retentionOverview.storage.total_bytes)}
              </span>
              <span className="settings-token-meta">
                {`会議 ${retentionOverview.meeting_count} / 成果物 ${retentionOverview.artifact_count}`}
              </span>
              <span className="settings-token-meta">
                {`Quota: ${retentionOverview.quota_readiness.enforcement_mode} / hard limitなし`}
              </span>
              <span className="settings-token-meta">
                {retentionOverview.legal_hold.supported
                  ? "Legal hold対応"
                  : "Legal hold未対応"}
              </span>
            </div>
          ) : null}

          {retentionCleanupPreview ? (
            <div className="settings-review-item">
              <div>
                <div className="settings-review-title">
                  {"削除候補"}
                  <span className="settings-review-meta">
                    {formatBytes(
                      retentionCleanupPreview.estimated_freed_bytes.total_bytes,
                    )}
                  </span>
                </div>
                <p>
                  {`Raw ${retentionCleanupPreview.raw_workspace_count} / Transcript ${retentionCleanupPreview.transcript_workspace_count} / Summary ${retentionCleanupPreview.summary_workspace_count}`}
                </p>
                <p className="settings-review-meta">
                  {`Expired artifacts ${retentionCleanupPreview.expired_artifact_count} / ${formatBytes(
                    retentionCleanupPreview.expired_artifact_bytes,
                  )}`}
                </p>
                {retentionCleanupRun ? (
                  <p className="settings-review-meta">
                    {`監査: ${
                      retentionCleanupRun.audit_recorded ? "記録済み" : "未記録"
                    } / transcripts ${retentionCleanupRun.report.transcripts_marked_deleted} / summaries ${retentionCleanupRun.report.summaries_deleted} / artifacts ${retentionCleanupRun.report.artifacts_deleted}`}
                  </p>
                ) : null}
                {retentionCleanupRun?.error ? (
                  <p className="settings-version-meta">
                    {retentionCleanupRun.error}
                  </p>
                ) : null}
              </div>
            </div>
          ) : null}

          <div className="settings-customization-panel">
            <div className="settings-customization-header">
              <div>
                <h3>{"会議コンテンツ削除"}</h3>
                <p>
                  {
                    "会議行、使用量履歴、監査履歴は残し、選択したアクティブコンテンツだけを削除します"
                  }
                </p>
              </div>
            </div>
            <label className="settings-field">
              <span>{"会議ID"}</span>
              <input
                type="text"
                value={retentionDraft.meeting_id}
                disabled={isSavingAny}
                onChange={(event) =>
                  updateRetentionDraft({ meeting_id: event.target.value })
                }
              />
            </label>
            <label className="settings-field">
              <span>{"理由"}</span>
              <input
                type="text"
                value={retentionDraft.reason}
                disabled={isSavingAny}
                onChange={(event) =>
                  updateRetentionDraft({ reason: event.target.value })
                }
              />
            </label>
            <div className="settings-token-actions">
              {(
                [
                  ["raw_audio", "Raw audio"],
                  ["transcript", "Transcript"],
                  ["summary", "Summary"],
                  ["debug", "Debug"],
                ] as const
              ).map(([target, label]) => (
                <label key={target} className="settings-checkbox">
                  <input
                    type="checkbox"
                    checked={retentionDraft.targets[target]}
                    disabled={isSavingAny}
                    onChange={(event) =>
                      updateRetentionTarget(target, event.target.checked)
                    }
                  />
                  <span>{label}</span>
                </label>
              ))}
            </div>
            <div className="settings-token-actions">
              <button
                type="button"
                className="secondary-button"
                disabled={
                  isSavingAny ||
                  retentionDraft.token.trim() === "" ||
                  retentionDraft.meeting_id.trim() === "" ||
                  !retentionTargetsSelected
                }
                onClick={handleRetentionMeetingPreview}
              >
                {activeOperation === "retention-meeting-preview"
                  ? "確認中"
                  : "削除確認"}
              </button>
              <button
                type="button"
                className="primary-button"
                disabled={
                  isSavingAny ||
                  retentionDraft.token.trim() === "" ||
                  retentionDraft.meeting_id.trim() === "" ||
                  !retentionTargetsSelected ||
                  !retentionMeetingPreviewMatchesDraft ||
                  !retentionMeetingPreviewStatusAllowsDelete
                }
                onClick={handleRetentionMeetingDelete}
              >
                {activeOperation === "retention-meeting-delete"
                  ? "削除中"
                  : "削除実行"}
              </button>
            </div>
            {retentionMeetingPreview ? (
              <p className="settings-version-meta">
                {`対象: ${retentionMeetingPreview.status} / 解放見込み ${formatBytes(
                  retentionMeetingPreview.estimated_freed_bytes.total_bytes,
                )} / 使用量履歴 ${retentionMeetingPreview.usage_event_count}件保持 / 監査履歴 ${retentionMeetingPreview.audit_event_count}件保持`}
              </p>
            ) : null}
            {retentionMeetingDelete ? (
              <p className="settings-version-meta">
                {`削除結果: transcripts ${retentionMeetingDelete.report.transcripts_marked_deleted} / summaries ${retentionMeetingDelete.report.summaries_deleted} / artifacts ${retentionMeetingDelete.report.artifacts_deleted} / 監査 ${
                  retentionMeetingDelete.audit_recorded ? "記録済み" : "未記録"
                }`}
              </p>
            ) : null}
          </div>

          {retentionError ? (
            <p className="settings-error">{retentionError}</p>
          ) : null}
        </section>
      ) : null}

      {settings && showCustomizations ? (
        <section className="settings-section settings-customization-section">
          <div className="settings-section-heading">
            <div>
              <h2>{"AI \u30ab\u30b9\u30bf\u30de\u30a4\u30ba"}</h2>
              <p>
                {
                  "要約に使うドメイン知識、AIメモ、フィードバック、人名別名、要約テンプレートを管理します"
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
                  aiMemoryDraft.id,
                  personAliasDraft.id,
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
              onSubmit={handleAiMemorySave}
            >
              <div className="settings-customization-header">
                <div>
                  <h3>{"AIメモ"}</h3>
                  <p>
                    {activeAiMemoryNotes.length > 0
                      ? `有効: ${activeAiMemoryNotes.length}件 / ピン留め: ${pinnedAiMemoryNotes.length}件`
                      : "有効なAIメモはありません"}
                  </p>
                </div>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={customizationControlsDisabled}
                  onClick={() => handleAiMemorySelect("")}
                >
                  {"新規"}
                </button>
              </div>

              <label className="settings-field">
                <span>{"メモ"}</span>
                <select
                  value={aiMemoryDraft.id ?? ""}
                  disabled={
                    customizationControlsDisabled || aiMemoryNotes.length === 0
                  }
                  onChange={(event) => handleAiMemorySelect(event.target.value)}
                >
                  <option value="">{"新規AIメモ"}</option>
                  {aiMemoryNotes.map((item) => (
                    <option key={item.id} value={item.id}>
                      {`${item.pinned ? "★ " : ""}${item.title} / ${labelFromRecord(
                        aiMemorySourceLabels,
                        item.source_type,
                      )}${
                        item.archived_at
                          ? " / アーカイブ済み"
                          : item.active
                            ? " / 有効"
                            : " / 無効"
                      }`}
                    </option>
                  ))}
                </select>
              </label>

              <label className="settings-field">
                <span>{"AIメモタイトル"}</span>
                <input
                  type="text"
                  value={aiMemoryDraft.title}
                  disabled={
                    customizationControlsDisabled ||
                    selectedAiMemoryNote?.archived_at != null
                  }
                  onChange={(event) =>
                    updateAiMemoryDraft({ title: event.target.value })
                  }
                />
              </label>

              <label className="settings-field">
                <span>{"AIメモ本文"}</span>
                <textarea
                  rows={6}
                  value={aiMemoryDraft.body}
                  disabled={
                    customizationControlsDisabled ||
                    selectedAiMemoryNote?.archived_at != null
                  }
                  onChange={(event) =>
                    updateAiMemoryDraft({ body: event.target.value })
                  }
                />
              </label>

              <label className="settings-field">
                <span>{"タグ"}</span>
                <input
                  type="text"
                  list="ai-memory-tags"
                  placeholder="terminology, summary_hint"
                  value={aiMemoryDraft.tagsText}
                  disabled={
                    customizationControlsDisabled ||
                    selectedAiMemoryNote?.archived_at != null
                  }
                  onChange={(event) =>
                    updateAiMemoryDraft({ tagsText: event.target.value })
                  }
                />
              </label>
              <datalist id="ai-memory-tags">
                {aiMemoryTags.map((tag) => (
                  <option key={tag} value={tag}>
                    {aiMemoryTagLabels[tag]}
                  </option>
                ))}
              </datalist>

              <label className="settings-field">
                <span>{"信頼度"}</span>
                <input
                  type="number"
                  min={0}
                  max={1}
                  step={0.001}
                  placeholder="0.8"
                  value={aiMemoryDraft.confidence}
                  disabled={
                    customizationControlsDisabled ||
                    selectedAiMemoryNote?.archived_at != null
                  }
                  onChange={(event) =>
                    updateAiMemoryDraft({ confidence: event.target.value })
                  }
                />
              </label>

              <label className="settings-checkbox">
                <input
                  type="checkbox"
                  checked={aiMemoryDraft.active}
                  disabled={
                    customizationControlsDisabled ||
                    selectedAiMemoryNote?.archived_at != null
                  }
                  onChange={(event) =>
                    updateAiMemoryDraft({ active: event.target.checked })
                  }
                />
                <span>{"有効にする"}</span>
              </label>

              <label className="settings-checkbox">
                <input
                  type="checkbox"
                  checked={aiMemoryDraft.pinned}
                  disabled={
                    customizationControlsDisabled ||
                    selectedAiMemoryNote?.archived_at != null
                  }
                  onChange={(event) =>
                    updateAiMemoryDraft({ pinned: event.target.checked })
                  }
                />
                <span>{"保存時にピン留めする"}</span>
              </label>

              {selectedAiMemoryNote ? (
                <p className="settings-version-meta">
                  {`${selectedAiMemoryNote.archived_at ? "アーカイブ済み" : selectedAiMemoryNote.active ? "有効" : "無効"} / ${
                    selectedAiMemoryNote.pinned ? "ピン留め" : "通常"
                  } / 更新: ${selectedAiMemoryNote.updated_at}`}
                </p>
              ) : null}

              <div className="settings-token-actions">
                <button
                  type="submit"
                  className="primary-button"
                  disabled={
                    customizationControlsDisabled ||
                    selectedAiMemoryNote?.archived_at != null
                  }
                >
                  {activeOperation === "memory-save"
                    ? "保存中"
                    : "AIメモを保存"}
                </button>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={
                    customizationControlsDisabled ||
                    !selectedAiMemoryNote ||
                    selectedAiMemoryNote.archived_at != null
                  }
                  onClick={handleAiMemoryPinToggle}
                >
                  {activeOperation === "memory-pin"
                    ? "更新中"
                    : selectedAiMemoryNote?.pinned
                      ? "ピン解除"
                      : "ピン留め"}
                </button>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={
                    customizationControlsDisabled ||
                    !selectedAiMemoryNote ||
                    selectedAiMemoryNote.archived_at != null
                  }
                  onClick={handleAiMemoryArchive}
                >
                  {activeOperation === "memory-archive"
                    ? "アーカイブ中"
                    : "アーカイブ"}
                </button>
              </div>

              <div className="settings-promote-row">
                <label className="settings-field">
                  <span>{"昇格先"}</span>
                  <select
                    value={aiMemoryDraft.promoteContentType}
                    disabled={
                      customizationControlsDisabled ||
                      !selectedAiMemoryNote ||
                      selectedAiMemoryNote.archived_at != null
                    }
                    onChange={(event) =>
                      updateAiMemoryDraft({
                        promoteContentType: event.target
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
                <button
                  type="button"
                  className="secondary-button"
                  disabled={
                    customizationControlsDisabled ||
                    !selectedAiMemoryNote ||
                    selectedAiMemoryNote.archived_at != null
                  }
                  onClick={handleAiMemoryPromote}
                >
                  {activeOperation === "memory-promote"
                    ? "昇格中"
                    : "ドメイン知識へ昇格"}
                </button>
              </div>
            </form>

            <div className="settings-customization-panel">
              <div className="settings-customization-header">
                <div>
                  <h3>{"フィードバックキュー"}</h3>
                  <p>
                    {feedbackItems.length > 0
                      ? `未対応: ${feedbackItems.length}件`
                      : "未対応のフィードバックはありません"}
                  </p>
                </div>
              </div>

              {feedbackItems.length > 0 ? (
                <ul className="settings-review-list">
                  {feedbackItems.map((item) => (
                    <li key={item.id} className="settings-review-item">
                      <div>
                        <div className="settings-review-title">
                          {labelFromRecord(
                            feedbackTypeLabels,
                            item.feedback_type,
                          )}
                          <span className="settings-review-meta">
                            {labelFromRecord(feedbackStatusLabels, item.status)}
                          </span>
                        </div>
                        <p>{feedbackText(item)}</p>
                        <p className="settings-review-meta">
                          {`作成: ${item.created_at}${
                            item.meeting_id ? ` / 会議: ${item.meeting_id}` : ""
                          }`}
                        </p>
                      </div>
                      <div className="settings-token-actions">
                        <button
                          type="button"
                          className="secondary-button"
                          aria-label={feedbackActionLabel(item, "採用")}
                          disabled={customizationControlsDisabled}
                          onClick={() =>
                            void handleFeedbackStatus(item, "accepted")
                          }
                        >
                          {"採用"}
                        </button>
                        <button
                          type="button"
                          className="secondary-button"
                          aria-label={feedbackActionLabel(item, "却下")}
                          disabled={customizationControlsDisabled}
                          onClick={() =>
                            void handleFeedbackStatus(item, "dismissed")
                          }
                        >
                          {"却下"}
                        </button>
                      </div>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="settings-version-meta">
                  {"会議ページから送信されたフィードバックがここに表示されます"}
                </p>
              )}
            </div>

            <form
              className="settings-customization-panel"
              onSubmit={handlePersonAliasSave}
            >
              <div className="settings-customization-header">
                <div>
                  <h3>{"人名別名"}</h3>
                  <p>
                    {activePersonAliases.length > 0
                      ? `有効: ${activePersonAliases.length}件`
                      : "有効な人名別名はありません"}
                  </p>
                </div>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={customizationControlsDisabled}
                  onClick={() => handlePersonAliasSelect("")}
                >
                  {"新規"}
                </button>
              </div>

              <label className="settings-field">
                <span>{"別名レコード"}</span>
                <select
                  value={personAliasDraft.id ?? ""}
                  disabled={
                    customizationControlsDisabled || personAliases.length === 0
                  }
                  onChange={(event) =>
                    handlePersonAliasSelect(event.target.value)
                  }
                >
                  <option value="">{"新規人名別名"}</option>
                  {personAliases.map((item) => (
                    <option key={item.id} value={item.id}>
                      {`${item.canonical_name} / ${item.alias} / ${
                        personAliasReviewStatusLabels[item.review_status]
                      }${
                        item.archived_at
                          ? " / アーカイブ済み"
                          : item.active
                            ? " / 有効"
                            : " / 無効"
                      }`}
                    </option>
                  ))}
                </select>
              </label>

              <label className="settings-field">
                <span>{"正式名"}</span>
                <input
                  type="text"
                  value={personAliasDraft.canonical_name}
                  disabled={
                    customizationControlsDisabled ||
                    selectedPersonAlias?.archived_at != null
                  }
                  onChange={(event) =>
                    updatePersonAliasDraft({
                      canonical_name: event.target.value,
                    })
                  }
                />
              </label>

              <label className="settings-field">
                <span>{"別名"}</span>
                <input
                  type="text"
                  value={personAliasDraft.alias}
                  disabled={
                    customizationControlsDisabled ||
                    selectedPersonAlias?.archived_at != null
                  }
                  onChange={(event) =>
                    updatePersonAliasDraft({ alias: event.target.value })
                  }
                />
              </label>

              <label className="settings-field">
                <span>{"DiscordユーザーID"}</span>
                <input
                  type="text"
                  value={personAliasDraft.discord_user_id}
                  disabled={
                    customizationControlsDisabled ||
                    selectedPersonAlias?.archived_at != null
                  }
                  onChange={(event) =>
                    updatePersonAliasDraft({
                      discord_user_id: event.target.value,
                    })
                  }
                />
              </label>

              <label className="settings-field">
                <span>{"信頼度"}</span>
                <input
                  type="number"
                  min={0}
                  max={1}
                  step={0.001}
                  value={personAliasDraft.confidence}
                  disabled={
                    customizationControlsDisabled ||
                    selectedPersonAlias?.archived_at != null
                  }
                  onChange={(event) =>
                    updatePersonAliasDraft({ confidence: event.target.value })
                  }
                />
              </label>

              <label className="settings-field">
                <span>{"レビュー状態"}</span>
                <select
                  value={personAliasDraft.review_status}
                  disabled={
                    customizationControlsDisabled ||
                    selectedPersonAlias?.archived_at != null
                  }
                  onChange={(event) =>
                    updatePersonAliasDraft({
                      review_status: event.target
                        .value as PersonAliasReviewStatus,
                    })
                  }
                >
                  {(
                    Object.keys(
                      personAliasReviewStatusLabels,
                    ) as PersonAliasReviewStatus[]
                  ).map((status) => (
                    <option key={status} value={status}>
                      {personAliasReviewStatusLabels[status]}
                    </option>
                  ))}
                </select>
              </label>

              <label className="settings-checkbox">
                <input
                  type="checkbox"
                  checked={personAliasDraft.active}
                  disabled={
                    customizationControlsDisabled ||
                    selectedPersonAlias?.archived_at != null
                  }
                  onChange={(event) =>
                    updatePersonAliasDraft({ active: event.target.checked })
                  }
                />
                <span>{"有効にする"}</span>
              </label>

              {selectedPersonAlias ? (
                <p className="settings-version-meta">
                  {`${labelFromRecord(
                    personAliasSourceLabels,
                    selectedPersonAlias.source_type,
                  )} / ${
                    selectedPersonAlias.archived_at
                      ? "アーカイブ済み"
                      : selectedPersonAlias.active
                        ? "有効"
                        : "無効"
                  } / 更新: ${selectedPersonAlias.updated_at}`}
                </p>
              ) : null}

              <div className="settings-token-actions">
                <button
                  type="submit"
                  className="primary-button"
                  disabled={
                    customizationControlsDisabled ||
                    selectedPersonAlias?.archived_at != null
                  }
                >
                  {activeOperation === "alias-save"
                    ? "保存中"
                    : "人名別名を保存"}
                </button>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={
                    customizationControlsDisabled ||
                    !selectedPersonAlias ||
                    selectedPersonAlias.archived_at != null
                  }
                  onClick={handlePersonAliasArchive}
                >
                  {activeOperation === "alias-archive"
                    ? "アーカイブ中"
                    : "アーカイブ"}
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
