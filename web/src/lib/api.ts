import type {
  DebugArtifact,
  GuildSettingsResponse,
  MeResponse,
  MeetingListResponse,
  MeetingResponse,
  SpeakerAudioInfo,
  SummaryResponse,
  TranscriptSegment,
  UpdateGuildSettingsRequest,
} from "./types";

function basePath(meetingId: string): string {
  return `/api/meetings/${encodeURIComponent(meetingId)}`;
}

function handleResponse<T>(response: Response): Promise<T> {
  if (response.status === 401) {
    window.location.href = `/auth/login?redirect=${encodeURIComponent(window.location.pathname + window.location.search + window.location.hash)}`;
    return new Promise(() => {});
  }
  if (!response.ok) {
    return Promise.reject(
      new Error(`${response.status} ${response.statusText}`),
    );
  }
  return response.json() as Promise<T>;
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
): Promise<TranscriptSegment[]> {
  return fetch(`${basePath(meetingId)}/transcript`, { signal }).then(
    handleResponse<TranscriptSegment[]>,
  );
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

// Guild dashboard API functions

export function fetchMe(signal?: AbortSignal): Promise<MeResponse> {
  return fetch("/api/me", { signal }).then(handleResponse<MeResponse>);
}

export function fetchGuildMeetings(
  page: number = 1,
  limit: number = 20,
  signal?: AbortSignal,
): Promise<MeetingListResponse> {
  const url = `/api/guild/meetings?page=${page}&limit=${limit}`;
  return fetch(url, { signal }).then(handleResponse<MeetingListResponse>);
}

export function fetchGuildSettings(
  signal?: AbortSignal,
): Promise<GuildSettingsResponse> {
  return fetch("/api/guild/settings", { signal }).then(
    handleResponse<GuildSettingsResponse>,
  );
}

export function updateGuildSettings(
  settings: UpdateGuildSettingsRequest,
): Promise<GuildSettingsResponse> {
  return fetch("/api/guild/settings", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(settings),
  }).then((response) => {
    if (response.status === 403) {
      return Promise.reject(new Error("forbidden"));
    }
    return handleResponse<GuildSettingsResponse>(response);
  });
}
