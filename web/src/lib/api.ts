import type {
  AiMemoryNote,
  AiMemoryPromoteRequest,
  AiMemorySourceType,
  AiMemoryUpsertRequest,
  DebugArtifact,
  DomainKnowledgeContentType,
  DomainKnowledgeItem,
  DomainKnowledgeUpsertRequest,
  GuildSettingsResponse,
  MeetingListResponse,
  MeetingResponse,
  MeResponse,
  PersonAlias,
  PersonAliasReviewStatus,
  PersonAliasUpsertRequest,
  SpeakerAudioInfo,
  SummaryResponse,
  SummaryTemplate,
  SummaryTemplateUpsertRequest,
  TranscriptFeedbackRequest,
  TranscriptFeedbackResponse,
  TranscriptFeedbackStatus,
  TranscriptFeedbackStatusRequest,
  TranscriptResponse,
  TranscriptSegment,
  TranscriptStateResponse,
  UpdateGuildBotTokenRequest,
  UpdateGuildSettingsRequest,
  UserGuild,
} from "./types";

function basePath(meetingId: string): string {
  return `/api/meetings/${encodeURIComponent(meetingId)}`;
}

function domainKnowledgePath(itemId?: string): string {
  const base = "/api/guild/domain-knowledge";
  return itemId ? `${base}/${encodeURIComponent(itemId)}` : base;
}

function aiMemoryPath(memoryId?: string): string {
  const base = "/api/guild/ai-memory";
  return memoryId ? `${base}/${encodeURIComponent(memoryId)}` : base;
}

function feedbackPath(feedbackId?: string): string {
  const base = "/api/guild/feedback";
  return feedbackId ? `${base}/${encodeURIComponent(feedbackId)}` : base;
}

function personAliasesPath(aliasId?: string): string {
  const base = "/api/guild/person-aliases";
  return aliasId ? `${base}/${encodeURIComponent(aliasId)}` : base;
}

function summaryTemplatePath(templateId?: string): string {
  const base = "/api/guild/summary-templates";
  return templateId ? `${base}/${encodeURIComponent(templateId)}` : base;
}

function guildSettingsPath(guildId?: string): string {
  return guildId
    ? `/api/guilds/${encodeURIComponent(guildId)}/settings`
    : "/api/guild/settings";
}

function guildBotTokenPath(guildId?: string): string {
  return `${guildSettingsPath(guildId)}/bot-token`;
}

function guildMeetingsPath(guildId?: string | null): string {
  return guildId
    ? `/api/guilds/${encodeURIComponent(guildId)}/meetings`
    : "/api/guild/meetings";
}

export function buildLoginRedirectUrl(path: string): string {
  return `/auth/login?redirect=${encodeURIComponent(path)}`;
}

function handleResponse<T>(response: Response): Promise<T> {
  if (response.status === 401) {
    window.location.href = buildLoginRedirectUrl(
      window.location.pathname + window.location.search + window.location.hash,
    );
    return new Promise(() => {});
  }
  if (!response.ok) {
    return Promise.reject(
      new Error(`${response.status} ${response.statusText}`),
    );
  }
  return response.json() as Promise<T>;
}

function handleGuildSettingsResponse(
  response: Response,
): Promise<GuildSettingsResponse> {
  if (response.status === 401) {
    return handleResponse<GuildSettingsResponse>(response);
  }
  if (!response.ok) {
    return response
      .clone()
      .json()
      .catch(() => null)
      .then((payload: unknown) => {
        const code =
          payload &&
          typeof payload === "object" &&
          "code" in payload &&
          typeof payload.code === "string"
            ? payload.code
            : null;
        if (response.status === 403 && (!code || code === "forbidden")) {
          throw new Error("forbidden");
        }
        throw new Error(code ?? `${response.status} ${response.statusText}`);
      });
  }
  return handleResponse<GuildSettingsResponse>(response);
}

function handleMeResponse(response: Response): Promise<MeResponse> {
  if (response.status === 403) {
    throw new Error("forbidden");
  }
  return handleResponse<MeResponse>(response);
}

function handleGuildMeetingsResponse(
  response: Response,
): Promise<MeetingListResponse> {
  if (response.status === 403) {
    throw new Error("forbidden");
  }
  return handleResponse<MeetingListResponse>(response);
}

export function fetchMe(signal?: AbortSignal): Promise<MeResponse> {
  return fetch("/api/me", { signal }).then(handleMeResponse);
}

export function fetchMeGuilds(signal?: AbortSignal): Promise<UserGuild[]> {
  return fetch("/api/me/guilds", { signal }).then(handleResponse<UserGuild[]>);
}

export function fetchGuildMeetings(
  guildId?: string | null,
  page = 1,
  limit = 20,
  voiceChannelId?: string | null,
  signal?: AbortSignal,
): Promise<MeetingListResponse> {
  const params = new URLSearchParams({
    page: String(page),
    limit: String(limit),
  });
  const normalizedVoiceChannelId = voiceChannelId?.trim();
  if (normalizedVoiceChannelId) {
    params.set("voice_channel_id", normalizedVoiceChannelId);
  }

  return fetch(`${guildMeetingsPath(guildId)}?${params.toString()}`, {
    signal,
  }).then(handleGuildMeetingsResponse);
}

export function fetchGuildSettings(
  guildId?: string,
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch(guildSettingsPath(guildId), { signal }).then(
    handleGuildSettingsResponse,
  );
}

export function updateGuildSettings(
  request: UpdateGuildSettingsRequest,
  guildId?: string,
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch(guildSettingsPath(guildId), {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleGuildSettingsResponse);
}

export function updateGuildBotToken(
  request: UpdateGuildBotTokenRequest,
  guildId?: string,
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch(guildBotTokenPath(guildId), {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleGuildSettingsResponse);
}

export function deleteGuildBotToken(
  guildId?: string,
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch(guildBotTokenPath(guildId), {
    method: "DELETE",
    signal,
  }).then(handleGuildSettingsResponse);
}

export function fetchDomainKnowledgeItems(
  options: {
    includeArchived?: boolean;
    contentType?: DomainKnowledgeContentType;
  } = {},
  signal?: AbortSignal,
): Promise<DomainKnowledgeItem[]> {
  const params = new URLSearchParams();
  if (options.includeArchived !== undefined) {
    params.set("include_archived", String(options.includeArchived));
  }
  if (options.contentType) {
    params.set("content_type", options.contentType);
  }
  const query = params.toString();
  const path = query
    ? `${domainKnowledgePath()}?${query}`
    : domainKnowledgePath();
  return fetch(path, { signal }).then(handleResponse<DomainKnowledgeItem[]>);
}

export function fetchDomainKnowledgeItem(
  itemId: string,
  signal?: AbortSignal,
): Promise<DomainKnowledgeItem> {
  return fetch(domainKnowledgePath(itemId), { signal }).then(
    handleResponse<DomainKnowledgeItem>,
  );
}

export function createDomainKnowledgeItem(
  request: DomainKnowledgeUpsertRequest,
  signal?: AbortSignal,
): Promise<DomainKnowledgeItem> {
  return fetch(domainKnowledgePath(), {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<DomainKnowledgeItem>);
}

export function updateDomainKnowledgeItem(
  itemId: string,
  request: DomainKnowledgeUpsertRequest,
  signal?: AbortSignal,
): Promise<DomainKnowledgeItem> {
  return fetch(domainKnowledgePath(itemId), {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<DomainKnowledgeItem>);
}

export function activateDomainKnowledgeItem(
  itemId: string,
  signal?: AbortSignal,
): Promise<DomainKnowledgeItem> {
  return fetch(`${domainKnowledgePath(itemId)}/activate`, {
    method: "POST",
    signal,
  }).then(handleResponse<DomainKnowledgeItem>);
}

export function archiveDomainKnowledgeItem(
  itemId: string,
  signal?: AbortSignal,
): Promise<DomainKnowledgeItem> {
  return fetch(`${domainKnowledgePath(itemId)}/archive`, {
    method: "POST",
    signal,
  }).then(handleResponse<DomainKnowledgeItem>);
}

export function fetchAiMemoryNotes(
  options: {
    includeArchived?: boolean;
    sourceType?: AiMemorySourceType;
  } = {},
  signal?: AbortSignal,
): Promise<AiMemoryNote[]> {
  const params = new URLSearchParams();
  if (options.includeArchived !== undefined) {
    params.set("include_archived", String(options.includeArchived));
  }
  if (options.sourceType) {
    params.set("source_type", options.sourceType);
  }
  const query = params.toString();
  const path = query ? `${aiMemoryPath()}?${query}` : aiMemoryPath();
  return fetch(path, { signal }).then(handleResponse<AiMemoryNote[]>);
}

export function createAiMemoryNote(
  request: AiMemoryUpsertRequest,
  signal?: AbortSignal,
): Promise<AiMemoryNote> {
  return fetch(aiMemoryPath(), {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<AiMemoryNote>);
}

export function updateAiMemoryNote(
  memoryId: string,
  request: AiMemoryUpsertRequest,
  signal?: AbortSignal,
): Promise<AiMemoryNote> {
  return fetch(aiMemoryPath(memoryId), {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<AiMemoryNote>);
}

export function pinAiMemoryNote(
  memoryId: string,
  signal?: AbortSignal,
): Promise<AiMemoryNote> {
  return fetch(`${aiMemoryPath(memoryId)}/pin`, {
    method: "POST",
    signal,
  }).then(handleResponse<AiMemoryNote>);
}

export function unpinAiMemoryNote(
  memoryId: string,
  signal?: AbortSignal,
): Promise<AiMemoryNote> {
  return fetch(`${aiMemoryPath(memoryId)}/unpin`, {
    method: "POST",
    signal,
  }).then(handleResponse<AiMemoryNote>);
}

export function archiveAiMemoryNote(
  memoryId: string,
  signal?: AbortSignal,
): Promise<AiMemoryNote> {
  return fetch(`${aiMemoryPath(memoryId)}/archive`, {
    method: "POST",
    signal,
  }).then(handleResponse<AiMemoryNote>);
}

export function promoteAiMemoryToDomainKnowledge(
  memoryId: string,
  request: AiMemoryPromoteRequest,
  signal?: AbortSignal,
): Promise<DomainKnowledgeItem> {
  return fetch(`${aiMemoryPath(memoryId)}/promote-to-domain-knowledge`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<DomainKnowledgeItem>);
}

export function fetchTranscriptFeedbackQueue(
  options: {
    status?: TranscriptFeedbackStatus;
    feedbackType?: string;
  } = {},
  signal?: AbortSignal,
): Promise<TranscriptFeedbackResponse[]> {
  const params = new URLSearchParams();
  if (options.status) {
    params.set("status", options.status);
  }
  if (options.feedbackType) {
    params.set("feedback_type", options.feedbackType);
  }
  const query = params.toString();
  const path = query ? `${feedbackPath()}?${query}` : feedbackPath();
  return fetch(path, { signal }).then(
    handleResponse<TranscriptFeedbackResponse[]>,
  );
}

export function updateTranscriptFeedbackStatus(
  feedbackId: string,
  request: TranscriptFeedbackStatusRequest,
  signal?: AbortSignal,
): Promise<TranscriptFeedbackResponse> {
  return fetch(`${feedbackPath(feedbackId)}/status`, {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<TranscriptFeedbackResponse>);
}

export function fetchPersonAliases(
  options: {
    includeArchived?: boolean;
    reviewStatus?: PersonAliasReviewStatus;
  } = {},
  signal?: AbortSignal,
): Promise<PersonAlias[]> {
  const params = new URLSearchParams();
  if (options.includeArchived !== undefined) {
    params.set("include_archived", String(options.includeArchived));
  }
  if (options.reviewStatus) {
    params.set("review_status", options.reviewStatus);
  }
  const query = params.toString();
  const path = query ? `${personAliasesPath()}?${query}` : personAliasesPath();
  return fetch(path, { signal }).then(handleResponse<PersonAlias[]>);
}

export function createPersonAlias(
  request: PersonAliasUpsertRequest,
  signal?: AbortSignal,
): Promise<PersonAlias> {
  return fetch(personAliasesPath(), {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<PersonAlias>);
}

export function updatePersonAlias(
  aliasId: string,
  request: PersonAliasUpsertRequest,
  signal?: AbortSignal,
): Promise<PersonAlias> {
  return fetch(personAliasesPath(aliasId), {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<PersonAlias>);
}

export function archivePersonAlias(
  aliasId: string,
  signal?: AbortSignal,
): Promise<PersonAlias> {
  return fetch(`${personAliasesPath(aliasId)}/archive`, {
    method: "POST",
    signal,
  }).then(handleResponse<PersonAlias>);
}

export function fetchSummaryTemplates(
  options: {
    includeArchived?: boolean;
  } = {},
  signal?: AbortSignal,
): Promise<SummaryTemplate[]> {
  const params = new URLSearchParams();
  if (options.includeArchived !== undefined) {
    params.set("include_archived", String(options.includeArchived));
  }
  const query = params.toString();
  const path = query
    ? `${summaryTemplatePath()}?${query}`
    : summaryTemplatePath();
  return fetch(path, { signal }).then(handleResponse<SummaryTemplate[]>);
}

export function fetchSummaryTemplate(
  templateId: string,
  signal?: AbortSignal,
): Promise<SummaryTemplate> {
  return fetch(summaryTemplatePath(templateId), { signal }).then(
    handleResponse<SummaryTemplate>,
  );
}

export function createSummaryTemplate(
  request: SummaryTemplateUpsertRequest,
  signal?: AbortSignal,
): Promise<SummaryTemplate> {
  return fetch(summaryTemplatePath(), {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<SummaryTemplate>);
}

export function updateSummaryTemplate(
  templateId: string,
  request: SummaryTemplateUpsertRequest,
  signal?: AbortSignal,
): Promise<SummaryTemplate> {
  return fetch(summaryTemplatePath(templateId), {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<SummaryTemplate>);
}

export function activateSummaryTemplate(
  templateId: string,
  signal?: AbortSignal,
): Promise<SummaryTemplate> {
  return fetch(`${summaryTemplatePath(templateId)}/activate`, {
    method: "POST",
    signal,
  }).then(handleResponse<SummaryTemplate>);
}

export function archiveSummaryTemplate(
  templateId: string,
  signal?: AbortSignal,
): Promise<SummaryTemplate> {
  return fetch(`${summaryTemplatePath(templateId)}/archive`, {
    method: "POST",
    signal,
  }).then(handleResponse<SummaryTemplate>);
}

export function fetchMeeting(
  meetingId: string,
  signal?: AbortSignal,
): Promise<MeetingResponse> {
  return fetch(basePath(meetingId), { signal }).then(
    handleResponse<MeetingResponse>,
  );
}

export function fetchTranscript(
  meetingId: string,
  signal?: AbortSignal,
): Promise<TranscriptResponse> {
  return fetch(`${basePath(meetingId)}/transcript`, { signal }).then(
    (response) =>
      handleResponse<TranscriptSegment[] | TranscriptResponse>(response).then(
        normalizeTranscriptResponse,
      ),
  );
}

export function fetchTranscriptState(
  meetingId: string,
  signal?: AbortSignal,
): Promise<TranscriptStateResponse> {
  return fetch(`${basePath(meetingId)}/transcript/state`, { signal }).then(
    handleResponse<TranscriptStateResponse>,
  );
}

export function normalizeTranscriptResponse(
  response: TranscriptSegment[] | TranscriptResponse,
): TranscriptResponse {
  if (Array.isArray(response)) {
    return {
      segments: response,
      status: "unknown",
      is_final: false,
      updated_at: null,
    };
  }
  return response;
}

export function getTranscriptEventsUrl(meetingId: string): string {
  return `${basePath(meetingId)}/transcript/events`;
}

export function createMeetingFeedback(
  meetingId: string,
  request: TranscriptFeedbackRequest,
  signal?: AbortSignal,
): Promise<TranscriptFeedbackResponse> {
  return fetch(`${basePath(meetingId)}/feedback`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<TranscriptFeedbackResponse>);
}

export function fetchSummary(
  meetingId: string,
  signal?: AbortSignal,
): Promise<SummaryResponse> {
  return fetch(`${basePath(meetingId)}/summary`, { signal }).then(
    handleResponse<SummaryResponse>,
  );
}

export function getAudioUrl(meetingId: string): string {
  return `${basePath(meetingId)}/audio`;
}

export function fetchSpeakers(
  meetingId: string,
  signal?: AbortSignal,
): Promise<SpeakerAudioInfo[]> {
  return fetch(`${basePath(meetingId)}/speakers`, { signal }).then(
    handleResponse<SpeakerAudioInfo[]>,
  );
}

export function getSpeakerAudioUrl(
  meetingId: string,
  speakerId: string,
): string {
  return `${basePath(meetingId)}/speakers/${encodeURIComponent(speakerId)}/audio`;
}

export function fetchDebugManifest(
  meetingId: string,
  signal?: AbortSignal,
): Promise<DebugArtifact[]> {
  return fetch(`${basePath(meetingId)}/debug/manifest`, { signal }).then(
    handleResponse<DebugArtifact[]>,
  );
}
