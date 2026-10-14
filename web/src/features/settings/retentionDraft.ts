import type {
  AdminRetentionPolicyRequest,
  AdminRetentionTargets,
} from "../../lib/types";

export interface RetentionAdminDraft {
  token: string;
  summary_ttl_days: string;
  meeting_id: string;
  reason: string;
  targets: AdminRetentionTargets;
}

interface RetentionSettingsDraft {
  retention_raw_audio_ttl_days: string;
  retention_transcript_ttl_days: string;
}

export function emptyRetentionAdminDraft(): RetentionAdminDraft {
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

export function retentionPolicyRequestFromDraft(
  draft: RetentionAdminDraft,
  form: RetentionSettingsDraft | null,
): AdminRetentionPolicyRequest {
  const summaryTtl = draft.summary_ttl_days.trim();
  return {
    raw_audio_ttl_days: form
      ? readInteger(form.retention_raw_audio_ttl_days)
      : undefined,
    transcript_ttl_days: form
      ? readInteger(form.retention_transcript_ttl_days)
      : undefined,
    summary_ttl_days: summaryTtl === "" ? undefined : readInteger(summaryTtl),
  };
}

export function validateRetentionPolicyDraft(
  draft: RetentionAdminDraft,
  form: RetentionSettingsDraft | null,
): string | null {
  if (!form) {
    return "設定を読み込んでから保持条件を確認してください";
  }
  const rawAudioTtlDays = readInteger(form.retention_raw_audio_ttl_days);
  const transcriptTtlDays = readInteger(form.retention_transcript_ttl_days);
  const summaryTtl = draft.summary_ttl_days.trim();
  const summaryTtlDays = summaryTtl === "" ? null : readInteger(summaryTtl);
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

export function retentionPolicyKey(
  request: AdminRetentionPolicyRequest,
): string {
  return [
    request.raw_audio_ttl_days ?? "",
    request.transcript_ttl_days ?? "",
    request.summary_ttl_days ?? "",
  ].join("|");
}

export function retentionMeetingDeleteRequest(draft: RetentionAdminDraft) {
  const reason = draft.reason.trim();
  return {
    targets: draft.targets,
    reason: reason === "" ? null : reason,
  };
}

export function retentionMeetingPreviewKeyFor(
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

export function sameRetentionTargets(
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

function readInteger(value: string): number {
  const parsed = Number(value);
  return Number.isInteger(parsed) ? parsed : Number.NaN;
}
