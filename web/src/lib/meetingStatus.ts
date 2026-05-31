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

export const STATUS_LABELS: Record<string, string> = {
  scheduled: "\u4e88\u5b9a",
  recording: "\u9332\u97f3\u4e2d",
  stopping: "\u505c\u6b62\u51e6\u7406\u4e2d",
  transcribing: "\u6587\u5b57\u8d77\u3053\u3057\u4e2d",
  summarizing: "\u8981\u7d04\u4e2d",
  posted: "\u5b8c\u4e86",
  failed: "\u5931\u6557",
  aborted: "\u4e2d\u6b62",
  processing: "\u51e6\u7406\u4e2d",
};

export function statusLabel(status: string): string {
  return STATUS_LABELS[status] ?? status;
}

export function statusClassName(status: string): string {
  return status.replace(/[^a-z0-9_-]/gi, "-").toLowerCase();
}
