import type {
  DebugArtifact,
  GuildSettingsResponse,
  MeetingListResponse,
  MeetingResponse,
  MeResponse,
  SpeakerAudioInfo,
  SummaryResponse,
  TranscriptResponse,
  TranscriptSegment,
  TranscriptStateResponse,
  UpdateGuildBotTokenRequest,
  UpdateGuildSettingsRequest,
} from "./types";

function basePath(meetingId: string): string {
  return `/api/meetings/${encodeURIComponent(meetingId)}`;
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

export function fetchGuildMeetings(
  page = 1,
  limit = 20,
  signal?: AbortSignal,
): Promise<MeetingListResponse> {
  const params = new URLSearchParams({
    page: String(page),
    limit: String(limit),
  });

  return fetch(`/api/guild/meetings?${params.toString()}`, { signal }).then(
    handleGuildMeetingsResponse,
  );
}

export function fetchGuildSettings(
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch("/api/guild/settings", { signal }).then(
    handleGuildSettingsResponse,
  );
}

export function updateGuildSettings(
  request: UpdateGuildSettingsRequest,
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch("/api/guild/settings", {
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
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch("/api/guild/settings/bot-token", {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  }).then(handleGuildSettingsResponse);
}

export function deleteGuildBotToken(
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch("/api/guild/settings/bot-token", {
    method: "DELETE",
    signal,
  }).then(handleGuildSettingsResponse);
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
