import { describe, expect, it } from "vitest";
import type { AdminRetentionTargets } from "../../lib/types";
import {
  emptyRetentionAdminDraft,
  retentionMeetingDeleteRequest,
  retentionMeetingPreviewKeyFor,
  retentionPolicyKey,
  retentionPolicyRequestFromDraft,
  sameRetentionTargets,
  validateRetentionPolicyDraft,
} from "./retentionDraft";

const settings = {
  retention_raw_audio_ttl_days: "14",
  retention_transcript_ttl_days: "60",
};

const allTargets: AdminRetentionTargets = {
  raw_audio: true,
  transcript: true,
  summary: true,
  debug: true,
};

describe("retention draft helpers", () => {
  it("starts with all cleanup targets selected and optional values blank", () => {
    expect(emptyRetentionAdminDraft()).toEqual({
      token: "",
      summary_ttl_days: "",
      meeting_id: "",
      reason: "",
      targets: allTargets,
    });
  });

  it("omits an empty summary TTL and unavailable settings from the request", () => {
    const draft = emptyRetentionAdminDraft();
    expect(retentionPolicyRequestFromDraft(draft, settings)).toEqual({
      raw_audio_ttl_days: 14,
      transcript_ttl_days: 60,
      summary_ttl_days: undefined,
    });
    expect(retentionPolicyRequestFromDraft(draft, null)).toEqual({
      raw_audio_ttl_days: undefined,
      transcript_ttl_days: undefined,
      summary_ttl_days: undefined,
    });
  });

  it("validates integer retention values from 1 through 365 days", () => {
    const draft = emptyRetentionAdminDraft();
    expect(validateRetentionPolicyDraft(draft, settings)).toBeNull();
    expect(
      validateRetentionPolicyDraft(
        { ...draft, summary_ttl_days: " 365 " },
        settings,
      ),
    ).toBeNull();
    expect(
      validateRetentionPolicyDraft(draft, {
        retention_raw_audio_ttl_days: "1",
        retention_transcript_ttl_days: "365",
      }),
    ).toBeNull();
    expect(
      validateRetentionPolicyDraft(draft, {
        ...settings,
        retention_raw_audio_ttl_days: "366",
      }),
    ).toBe("保持日数は1から365の整数で入力してください");
    expect(
      validateRetentionPolicyDraft(
        { ...draft, summary_ttl_days: "1.5" },
        settings,
      ),
    ).toBe("保持日数は1から365の整数で入力してください");
    expect(validateRetentionPolicyDraft(draft, null)).toBe(
      "設定を読み込んでから保持条件を確認してください",
    );
  });

  it("keeps stable keys for policy previews and differentiates targets", () => {
    expect(
      retentionPolicyKey({
        raw_audio_ttl_days: 14,
        transcript_ttl_days: 60,
      }),
    ).toBe("14|60|");
    expect(retentionMeetingPreviewKeyFor(" meeting-1 ", allTargets)).toBe(
      " meeting-1 |raw|transcript|summary|debug",
    );
    expect(sameRetentionTargets(allTargets, { ...allTargets })).toBe(true);
    expect(
      sameRetentionTargets(allTargets, { ...allTargets, debug: false }),
    ).toBe(false);
  });

  it("trims deletion reasons and uses null for a blank reason", () => {
    const draft = emptyRetentionAdminDraft();
    expect(retentionMeetingDeleteRequest(draft)).toEqual({
      targets: allTargets,
      reason: null,
    });
    expect(
      retentionMeetingDeleteRequest({ ...draft, reason: "  incident  " }),
    ).toEqual({ targets: allTargets, reason: "incident" });
  });
});
