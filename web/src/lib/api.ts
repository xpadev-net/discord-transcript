import type { MeetingResponse, TranscriptSegment, SummaryResponse } from "./types";

function basePath(meetingId: string): string {
  return `/api/meetings/${encodeURIComponent(meetingId)}`;
}

function handleResponse(response: Response): Promise<unknown> {
  if (response.status === 401) {
    window.location.href = `/auth/login?redirect=${encodeURIComponent(window.location.pathname)}`;
    return Promise.reject(new Error("Unauthorized"));
  }
  if (!response.ok) {
    return Promise.reject(new Error(`${response.status} ${response.statusText}`));
  }
  return response.json();
}

export function fetchMeeting(meetingId: string): Promise<MeetingResponse> {
  return fetch(basePath(meetingId)).then(handleResponse) as Promise<MeetingResponse>;
}

export function fetchTranscript(meetingId: string): Promise<TranscriptSegment[]> {
  return fetch(`${basePath(meetingId)}/transcript`).then(handleResponse) as Promise<TranscriptSegment[]>;
}

export function fetchSummary(meetingId: string): Promise<SummaryResponse> {
  return fetch(`${basePath(meetingId)}/summary`).then(handleResponse) as Promise<SummaryResponse>;
}

export function getAudioUrl(meetingId: string): string {
  return `${basePath(meetingId)}/audio`;
}
