import type {
  AdminGuildPlanAssignment,
  AdminGuildPlanAssignmentCreateRequest,
  AdminGuildPlanAssignmentUpsertRequest,
  AdminPlan,
  AdminPlanQuota,
  AdminPlanQuotaUpsertRequest,
  AdminPlanUpsertRequest,
  AdminRetentionCleanupPreview,
  AdminRetentionCleanupRun,
  AdminRetentionMeetingDelete,
  AdminRetentionMeetingDeletePreview,
  AdminRetentionMeetingDeleteRequest,
  AdminRetentionOverview,
  AdminRetentionPolicyRequest,
  AiMemoryNote,
  AiMemoryPromoteRequest,
  AiMemorySourceType,
  AiMemoryUpsertRequest,
  DebugArtifact,
  DomainKnowledgeContentType,
  DomainKnowledgeItem,
  DomainKnowledgeUpsertRequest,
  GuildJob,
  GuildJobStatus,
  GuildJobType,
  GuildRbacManagement,
  GuildRbacRoleGrant,
  GuildRbacRoleGrantUpdateRequest,
  GuildSettingsResponse,
  MeetingListItem,
  MeetingListResponse,
  MeetingResponse,
  MeetingVoiceChannel,
  MeResponse,
  PersonAlias,
  PersonAliasReviewStatus,
  PersonAliasUpsertRequest,
  SpeakerAudioInfo,
  SpeakerResponse,
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

function meetingReadPath(
  meetingId: string,
  guildId?: string,
  suffix = "",
): string {
  const path = `${basePath(meetingId)}${suffix}`;
  return guildId ? `${path}?guild_id=${encodeURIComponent(guildId)}` : path;
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

function guildRbacPath(guildId?: string): string {
  return guildId
    ? `/api/guilds/${encodeURIComponent(guildId)}/rbac`
    : "/api/guild/rbac";
}

function guildRbacRolePath(
  guildId: string | undefined,
  roleId: string,
): string {
  return `${guildRbacPath(guildId)}/roles/${encodeURIComponent(roleId)}`;
}

function guildMeetingsPath(guildId?: string | null): string {
  return guildId
    ? `/api/guilds/${encodeURIComponent(guildId)}/meetings`
    : "/api/guild/meetings";
}

function guildJobsPath(): string {
  return "/api/guild/jobs";
}

function adminPlanPath(planId?: string): string {
  return planId
    ? `/api/admin/plans/${encodeURIComponent(planId)}`
    : "/api/admin/plans";
}

function adminPlanQuotaPath(quotaId?: string): string {
  return quotaId
    ? `/api/admin/quotas/${encodeURIComponent(quotaId)}`
    : "/api/admin/quotas";
}

function adminPlanQuotasPath(planId: string): string {
  return `${adminPlanPath(planId)}/quotas`;
}

function adminGuildPlanAssignmentPath(assignmentId?: string): string {
  return assignmentId
    ? `/api/admin/guild-plan-assignments/${encodeURIComponent(assignmentId)}`
    : "/api/admin/guild-plan-assignments";
}

function adminRetentionMeetingPath(meetingId: string, action: string): string {
  return `/api/admin/retention/meetings/${encodeURIComponent(meetingId)}/${action}`;
}

export function buildLoginRedirectUrl(path: string): string {
  return `/auth/login?redirect=${encodeURIComponent(path)}`;
}

export class ApiError extends Error {
  readonly status: number;
  readonly code: string | null;

  constructor(status: number, code: string | null, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

export class ApiResponseValidationError extends Error {
  readonly code = "response_validation";

  constructor(message: string) {
    super(message);
    this.name = "ApiResponseValidationError";
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError;
}

export function isApiResponseValidationError(
  error: unknown,
): error is ApiResponseValidationError {
  return error instanceof ApiResponseValidationError;
}

function responseApiError(response: Response): Promise<ApiError> {
  return response
    .clone()
    .json()
    .catch(() => null)
    .then((payload: unknown) => {
      const responseCode =
        payload &&
        typeof payload === "object" &&
        "code" in payload &&
        typeof payload.code === "string"
          ? payload.code
          : null;
      const code = responseCode?.trim() ? responseCode : null;
      return new ApiError(
        response.status,
        code,
        `${response.status} ${response.statusText}`,
      );
    });
}

function rejectApiResponse(response: Response): Promise<never> {
  return responseApiError(response).then((error) => Promise.reject(error));
}

function rejectForbiddenResponse(response: Response): Promise<never> {
  return responseApiError(response).then((error) =>
    Promise.reject(new ApiError(403, error.code ?? "forbidden", "forbidden")),
  );
}

function handleResponse<T>(response: Response): Promise<T> {
  if (response.status === 401) {
    window.location.href = buildLoginRedirectUrl(
      window.location.pathname + window.location.search + window.location.hash,
    );
    return rejectApiResponse(response);
  }
  if (!response.ok) {
    return rejectApiResponse(response);
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
    return responseApiError(response).then((error) => {
      if (
        response.status === 403 &&
        (!error.code || error.code === "forbidden")
      ) {
        throw new ApiError(
          response.status,
          error.code ?? "forbidden",
          "forbidden",
        );
      }
      if (error.code) {
        throw new ApiError(response.status, error.code, error.code);
      }
      throw error;
    });
  }
  return handleResponse<GuildSettingsResponse>(response);
}

function handleMeResponse(response: Response): Promise<MeResponse> {
  if (response.status === 403) {
    return rejectForbiddenResponse(response);
  }
  return handleResponse<MeResponse>(response);
}

function handleAdminResponse<T>(response: Response): Promise<T> {
  if (!response.ok) {
    return rejectApiResponse(response);
  }
  return response.json() as Promise<T>;
}

function invalidMeetingListResponse(): ApiResponseValidationError {
  return new ApiResponseValidationError("会議一覧のレスポンス形式が不正です");
}

class TranscriptResponseValidationError extends ApiResponseValidationError {
  constructor() {
    super("文字起こしのレスポンス形式が不正です");
    this.name = "TranscriptResponseValidationError";
  }
}

function invalidTranscriptResponse(): TranscriptResponseValidationError {
  return new TranscriptResponseValidationError();
}

export function isTranscriptResponseValidationError(
  error: unknown,
): error is TranscriptResponseValidationError {
  return error instanceof TranscriptResponseValidationError;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isNullableNumber(value: unknown): value is number | null {
  return (
    value === null || (typeof value === "number" && Number.isFinite(value))
  );
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isTranscriptSegmentSource(
  value: unknown,
): value is TranscriptSegment["source"] {
  return value === "voice" || value === "vc_text";
}

function parseMeetingListItem(value: unknown): MeetingListItem | null {
  if (!isRecord(value)) {
    return null;
  }
  if (
    typeof value.id !== "string" ||
    !isNullableString(value.title) ||
    typeof value.status !== "string" ||
    !isNullableString(value.started_at) ||
    !isNullableString(value.stopped_at) ||
    !isNullableNumber(value.duration_seconds) ||
    !isNullableString(value.stop_reason) ||
    typeof value.voice_channel_id !== "string" ||
    !isNullableString(value.voice_channel_name)
  ) {
    return null;
  }
  return {
    id: value.id,
    title: value.title,
    status: value.status,
    started_at: value.started_at,
    stopped_at: value.stopped_at,
    duration_seconds: value.duration_seconds,
    stop_reason: value.stop_reason,
    voice_channel_id: value.voice_channel_id,
    voice_channel_name: value.voice_channel_name,
  };
}

function parseMeetingVoiceChannel(value: unknown): MeetingVoiceChannel | null {
  if (!isRecord(value)) {
    return null;
  }
  if (typeof value.id !== "string" || typeof value.label !== "string") {
    return null;
  }
  return { id: value.id, label: value.label };
}

function parseMeetingListResponse(payload: unknown): MeetingListResponse {
  if (
    !isRecord(payload) ||
    typeof payload.guild_id !== "string" ||
    !Array.isArray(payload.meetings) ||
    !Array.isArray(payload.voice_channels) ||
    !isFiniteNumber(payload.page) ||
    !isFiniteNumber(payload.limit) ||
    !(payload.total === null || isFiniteNumber(payload.total)) ||
    !(
      payload.next_cursor === undefined || isNullableString(payload.next_cursor)
    )
  ) {
    throw invalidMeetingListResponse();
  }
  const meetings: MeetingListItem[] = [];
  for (const meeting of payload.meetings) {
    const parsed = parseMeetingListItem(meeting);
    if (!parsed) {
      throw invalidMeetingListResponse();
    }
    meetings.push(parsed);
  }

  const voiceChannels: MeetingVoiceChannel[] = [];
  for (const channel of payload.voice_channels) {
    const parsed = parseMeetingVoiceChannel(channel);
    if (!parsed) {
      throw invalidMeetingListResponse();
    }
    voiceChannels.push(parsed);
  }

  return {
    guild_id: payload.guild_id,
    meetings,
    voice_channels: voiceChannels,
    page: payload.page,
    limit: payload.limit,
    total: payload.total === null ? null : Number(payload.total),
    next_cursor: isNullableString(payload.next_cursor)
      ? payload.next_cursor
      : null,
  };
}

function handleGuildMeetingsResponse(
  response: Response,
): Promise<MeetingListResponse> {
  if (response.status === 403) {
    return rejectForbiddenResponse(response);
  }
  return handleResponse<unknown>(response)
    .then(parseMeetingListResponse)
    .catch((error: unknown) => {
      if (error instanceof SyntaxError) {
        throw invalidMeetingListResponse();
      }
      throw error;
    });
}

function handleGuildJobsResponse(response: Response): Promise<GuildJob[]> {
  if (response.status === 403) {
    return rejectForbiddenResponse(response);
  }
  return handleResponse<GuildJob[]>(response);
}

export interface AdminRequestOptions {
  bearerToken?: string;
  signal?: AbortSignal;
}

function adminHeaders(
  options: AdminRequestOptions,
  jsonBody = false,
): Record<string, string> {
  const headers: Record<string, string> = {};
  if (options.bearerToken) {
    headers.Authorization = `Bearer ${options.bearerToken}`;
  }
  if (jsonBody) {
    headers["Content-Type"] = "application/json";
  }
  return headers;
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
  pagination?: { mode: "cursor"; cursor: string | null },
): Promise<MeetingListResponse> {
  const params = new URLSearchParams(
    pagination
      ? { limit: String(limit) }
      : { page: String(page), limit: String(limit) },
  );
  if (pagination) {
    params.set("pagination", pagination.mode);
    params.set("page", String(page));
    if (pagination.cursor) params.set("cursor", pagination.cursor);
  }
  const normalizedVoiceChannelId = voiceChannelId?.trim();
  if (normalizedVoiceChannelId) {
    params.set("voice_channel_id", normalizedVoiceChannelId);
  }

  return fetch(`${guildMeetingsPath(guildId)}?${params.toString()}`, {
    signal,
  }).then(handleGuildMeetingsResponse);
}

export function fetchGuildJobs(
  options: {
    status?: GuildJobStatus | "";
    jobType?: GuildJobType | "";
    limit?: number;
    signal?: AbortSignal;
  } = {},
): Promise<GuildJob[]> {
  const params = new URLSearchParams();
  if (options.status) {
    params.set("status", options.status);
  }
  if (options.jobType) {
    params.set("job_type", options.jobType);
  }
  if (options.limit !== undefined) {
    params.set("limit", String(options.limit));
  }
  const query = params.toString();
  const path = query ? `${guildJobsPath()}?${query}` : guildJobsPath();
  return fetch(path, { signal: options.signal }).then(handleGuildJobsResponse);
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

export function fetchGuildRbac(
  guildId?: string,
  signal?: AbortSignal,
): Promise<GuildRbacManagement> {
  return fetch(guildRbacPath(guildId), { signal }).then(
    handleResponse<GuildRbacManagement>,
  );
}

export function updateGuildRbacRoleGrant(
  roleId: string,
  request: GuildRbacRoleGrantUpdateRequest,
  guildId?: string,
  signal?: AbortSignal,
): Promise<GuildRbacRoleGrant> {
  return fetch(guildRbacRolePath(guildId, roleId), {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleResponse<GuildRbacRoleGrant>);
}

export function resetGuildRbacRoleGrant(
  roleId: string,
  guildId?: string,
  signal?: AbortSignal,
): Promise<GuildRbacRoleGrant> {
  return fetch(guildRbacRolePath(guildId, roleId), {
    method: "DELETE",
    signal,
  }).then(handleResponse<GuildRbacRoleGrant>);
}

export function fetchAdminPlans(
  options: AdminRequestOptions = {},
): Promise<AdminPlan[]> {
  return fetch(adminPlanPath(), {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlan[]>);
}

export function fetchAdminPlan(
  planId: string,
  options: AdminRequestOptions = {},
): Promise<AdminPlan> {
  return fetch(adminPlanPath(planId), {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlan>);
}

export function fetchAdminDefaultPlan(
  options: AdminRequestOptions = {},
): Promise<AdminPlan> {
  return fetch("/api/admin/plans/default", {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlan>);
}

export function fetchAdminBetaPlan(
  options: AdminRequestOptions = {},
): Promise<AdminPlan> {
  return fetch("/api/admin/plans/beta", {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlan>);
}

export function createAdminPlan(
  request: AdminPlanUpsertRequest,
  options: AdminRequestOptions = {},
): Promise<AdminPlan> {
  return fetch(adminPlanPath(), {
    method: "POST",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlan>);
}

export function updateAdminPlan(
  planId: string,
  request: AdminPlanUpsertRequest,
  options: AdminRequestOptions = {},
): Promise<AdminPlan> {
  return fetch(adminPlanPath(planId), {
    method: "PUT",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlan>);
}

export function archiveAdminPlan(
  planId: string,
  options: AdminRequestOptions = {},
): Promise<AdminPlan> {
  return fetch(`${adminPlanPath(planId)}/archive`, {
    method: "POST",
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlan>);
}

export function fetchAdminPlanQuotas(
  planId: string,
  options: AdminRequestOptions = {},
): Promise<AdminPlanQuota[]> {
  return fetch(adminPlanQuotasPath(planId), {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlanQuota[]>);
}

export function fetchAdminPlanQuota(
  quotaId: string,
  options: AdminRequestOptions = {},
): Promise<AdminPlanQuota> {
  return fetch(adminPlanQuotaPath(quotaId), {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlanQuota>);
}

export function createAdminPlanQuota(
  planId: string,
  request: AdminPlanQuotaUpsertRequest,
  options: AdminRequestOptions = {},
): Promise<AdminPlanQuota> {
  return fetch(adminPlanQuotasPath(planId), {
    method: "POST",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlanQuota>);
}

export function updateAdminPlanQuota(
  quotaId: string,
  request: AdminPlanQuotaUpsertRequest,
  options: AdminRequestOptions = {},
): Promise<AdminPlanQuota> {
  return fetch(adminPlanQuotaPath(quotaId), {
    method: "PUT",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlanQuota>);
}

export function deleteAdminPlanQuota(
  quotaId: string,
  options: AdminRequestOptions = {},
): Promise<AdminPlanQuota> {
  return fetch(adminPlanQuotaPath(quotaId), {
    method: "DELETE",
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminPlanQuota>);
}

export function fetchAdminGuildPlanAssignments(
  options: AdminRequestOptions & {
    guildId?: string;
    tenantId?: string;
    includeArchived?: boolean;
    limit?: number;
  } = {},
): Promise<AdminGuildPlanAssignment[]> {
  const params = new URLSearchParams();
  if (options.guildId) {
    params.set("guild_id", options.guildId);
  }
  if (options.tenantId) {
    params.set("tenant_id", options.tenantId);
  }
  if (options.includeArchived !== undefined) {
    params.set("include_archived", String(options.includeArchived));
  }
  if (options.limit !== undefined) {
    params.set("limit", String(options.limit));
  }
  const query = params.toString();
  const path = query
    ? `${adminGuildPlanAssignmentPath()}?${query}`
    : adminGuildPlanAssignmentPath();
  return fetch(path, {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminGuildPlanAssignment[]>);
}

export function fetchAdminGuildPlanAssignment(
  assignmentId: string,
  options: AdminRequestOptions = {},
): Promise<AdminGuildPlanAssignment> {
  return fetch(adminGuildPlanAssignmentPath(assignmentId), {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminGuildPlanAssignment>);
}

export function createAdminGuildPlanAssignment(
  request: AdminGuildPlanAssignmentCreateRequest,
  options: AdminRequestOptions = {},
): Promise<AdminGuildPlanAssignment> {
  return fetch(adminGuildPlanAssignmentPath(), {
    method: "POST",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminGuildPlanAssignment>);
}

export function updateAdminGuildPlanAssignment(
  assignmentId: string,
  request: AdminGuildPlanAssignmentUpsertRequest,
  options: AdminRequestOptions = {},
): Promise<AdminGuildPlanAssignment> {
  return fetch(adminGuildPlanAssignmentPath(assignmentId), {
    method: "PUT",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminGuildPlanAssignment>);
}

export function archiveAdminGuildPlanAssignment(
  assignmentId: string,
  options: AdminRequestOptions = {},
): Promise<AdminGuildPlanAssignment> {
  return fetch(`${adminGuildPlanAssignmentPath(assignmentId)}/archive`, {
    method: "POST",
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminGuildPlanAssignment>);
}

export function fetchAdminRetentionOverview(
  options: AdminRequestOptions = {},
): Promise<AdminRetentionOverview> {
  return fetch("/api/admin/retention", {
    headers: adminHeaders(options),
    signal: options.signal,
  }).then(handleAdminResponse<AdminRetentionOverview>);
}

export function previewAdminRetentionCleanup(
  request: AdminRetentionPolicyRequest = {},
  options: AdminRequestOptions = {},
): Promise<AdminRetentionCleanupPreview> {
  return fetch("/api/admin/retention/cleanup-preview", {
    method: "POST",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminRetentionCleanupPreview>);
}

export function runAdminRetentionCleanup(
  request: AdminRetentionPolicyRequest = {},
  options: AdminRequestOptions = {},
): Promise<AdminRetentionCleanupRun> {
  return fetch("/api/admin/retention/cleanup-run", {
    method: "POST",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminRetentionCleanupRun>);
}

export function previewAdminRetentionMeetingDelete(
  meetingId: string,
  request: AdminRetentionMeetingDeleteRequest,
  options: AdminRequestOptions = {},
): Promise<AdminRetentionMeetingDeletePreview> {
  return fetch(adminRetentionMeetingPath(meetingId, "delete-preview"), {
    method: "POST",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminRetentionMeetingDeletePreview>);
}

export function runAdminRetentionMeetingDelete(
  meetingId: string,
  request: AdminRetentionMeetingDeleteRequest,
  options: AdminRequestOptions = {},
): Promise<AdminRetentionMeetingDelete> {
  return fetch(adminRetentionMeetingPath(meetingId, "delete"), {
    method: "POST",
    headers: adminHeaders(options, true),
    body: JSON.stringify(request),
    signal: options.signal,
  }).then(handleAdminResponse<AdminRetentionMeetingDelete>);
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
  guildId?: string,
): Promise<MeetingResponse> {
  return fetch(meetingReadPath(meetingId, guildId), { signal }).then(
    handleResponse<MeetingResponse>,
  );
}

function parseSpeakerResponse(value: unknown): SpeakerResponse | null {
  if (!isRecord(value)) {
    return null;
  }
  if (
    typeof value.id !== "string" ||
    !isNullableString(value.username) ||
    !isNullableString(value.nickname) ||
    !isNullableString(value.display_name) ||
    typeof value.display_label !== "string"
  ) {
    return null;
  }
  return {
    id: value.id,
    username: value.username,
    nickname: value.nickname,
    display_name: value.display_name,
    display_label: value.display_label,
  };
}

function parseTranscriptSegment(value: unknown): TranscriptSegment | null {
  if (!isRecord(value)) {
    return null;
  }
  const speaker = parseSpeakerResponse(value.speaker);
  if (
    (value.id !== undefined && typeof value.id !== "string") ||
    typeof value.speaker_id !== "string" ||
    !speaker ||
    !isFiniteNumber(value.start_ms) ||
    !isFiniteNumber(value.end_ms) ||
    typeof value.text !== "string" ||
    !isNullableNumber(value.confidence) ||
    typeof value.is_noisy !== "boolean" ||
    !isTranscriptSegmentSource(value.source)
  ) {
    return null;
  }

  const segment: TranscriptSegment = {
    speaker_id: value.speaker_id,
    speaker,
    start_ms: value.start_ms,
    end_ms: value.end_ms,
    text: value.text,
    confidence: value.confidence,
    is_noisy: value.is_noisy,
    source: value.source,
  };
  if (typeof value.id === "string") {
    segment.id = value.id;
  }
  return segment;
}

function parseTranscriptSegments(payload: unknown[]): TranscriptSegment[] {
  const segments: TranscriptSegment[] = [];
  for (const segment of payload) {
    const parsed = parseTranscriptSegment(segment);
    if (!parsed) {
      throw invalidTranscriptResponse();
    }
    segments.push(parsed);
  }
  return segments;
}

export function fetchTranscript(
  meetingId: string,
  signal?: AbortSignal,
  guildId?: string,
): Promise<TranscriptResponse> {
  return fetch(meetingReadPath(meetingId, guildId, "/transcript"), {
    signal,
  }).then((response) =>
    handleResponse<unknown>(response)
      .then(normalizeTranscriptResponse)
      .catch((error: unknown) => {
        if (error instanceof SyntaxError) {
          throw invalidTranscriptResponse();
        }
        throw error;
      }),
  );
}

export function fetchTranscriptState(
  meetingId: string,
  signal?: AbortSignal,
  guildId?: string,
): Promise<TranscriptStateResponse> {
  return fetch(
    `${basePath(meetingId)}/transcript/state${guildId ? `?guild_id=${encodeURIComponent(guildId)}` : ""}`,
    { signal },
  ).then(handleResponse<TranscriptStateResponse>);
}

export function normalizeTranscriptResponse(
  response: unknown,
): TranscriptResponse {
  if (Array.isArray(response)) {
    return {
      segments: parseTranscriptSegments(response),
      status: "unknown",
      is_final: false,
      reset: false,
      updated_at: null,
    };
  }
  if (
    !isRecord(response) ||
    !Array.isArray(response.segments) ||
    typeof response.status !== "string" ||
    typeof response.is_final !== "boolean" ||
    (response.reset !== undefined && typeof response.reset !== "boolean") ||
    !isNullableString(response.updated_at)
  ) {
    throw invalidTranscriptResponse();
  }
  return {
    segments: parseTranscriptSegments(response.segments),
    status: response.status,
    is_final: response.is_final,
    reset: response.reset === true,
    updated_at: response.updated_at,
  };
}

export function getTranscriptEventsUrl(
  meetingId: string,
  guildId?: string,
): string {
  return `${basePath(meetingId)}/transcript/events${guildId ? `?guild_id=${encodeURIComponent(guildId)}` : ""}`;
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
  guildId?: string,
): Promise<SummaryResponse> {
  return fetch(
    `${basePath(meetingId)}/summary${guildId ? `?guild_id=${encodeURIComponent(guildId)}` : ""}`,
    { signal },
  ).then(handleResponse<SummaryResponse>);
}

export function getAudioUrl(meetingId: string, guildId?: string): string {
  return `${basePath(meetingId)}/audio${guildId ? `?guild_id=${encodeURIComponent(guildId)}` : ""}`;
}

export function fetchSpeakers(
  meetingId: string,
  signal?: AbortSignal,
  guildId?: string,
): Promise<SpeakerAudioInfo[]> {
  return fetch(
    `${basePath(meetingId)}/speakers${guildId ? `?guild_id=${encodeURIComponent(guildId)}` : ""}`,
    { signal },
  ).then(handleResponse<SpeakerAudioInfo[]>);
}

export function getSpeakerAudioUrl(
  meetingId: string,
  speakerId: string,
  guildId?: string,
): string {
  return `${basePath(meetingId)}/speakers/${encodeURIComponent(speakerId)}/audio${guildId ? `?guild_id=${encodeURIComponent(guildId)}` : ""}`;
}

export function fetchDebugManifest(
  meetingId: string,
  signal?: AbortSignal,
  guildId?: string,
): Promise<DebugArtifact[]> {
  return fetch(
    `${basePath(meetingId)}/debug/manifest${guildId ? `?guild_id=${encodeURIComponent(guildId)}` : ""}`,
    { signal },
  ).then(handleResponse<DebugArtifact[]>);
}
