export const LIVE_MEETING_STATUSES = new Set([
  "recording",
  "stopping",
  "transcribing",
  "summarizing",
  "processing",
]);

export function isLiveMeetingStatus(status: string | undefined): boolean {
  return status != null && LIVE_MEETING_STATUSES.has(status);
}
