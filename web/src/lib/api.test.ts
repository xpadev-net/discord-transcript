import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  ApiResponseValidationError,
  fetchAdminRetentionOverview,
  fetchGuildMeetings,
  fetchTranscript,
} from "./api";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("typed API errors", () => {
  it.each([
    401, 403, 404, 429, 500, 503,
  ])("preserves HTTP %s status and machine code", async (status) => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          Response.json({ code: `status_${status}` }, { status }),
        ),
    );

    await expect(fetchAdminRetentionOverview()).rejects.toMatchObject({
      name: "ApiError",
      status,
      code: `status_${status}`,
    });
  });

  it("falls back to the HTTP status text when an error has no code", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response("not json", {
          status: 429,
          statusText: "Too Many Requests",
        }),
      ),
    );

    await expect(fetchAdminRetentionOverview()).rejects.toMatchObject({
      status: 429,
      code: null,
      message: "429 Too Many Requests",
    });
  });

  it("keeps network failures and aborts separate from HTTP errors", async () => {
    const networkFailure = new TypeError("offline");
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(networkFailure));
    await expect(fetchAdminRetentionOverview()).rejects.toBe(networkFailure);

    const abortError = new DOMException("Aborted", "AbortError");
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(abortError));
    await expect(fetchAdminRetentionOverview()).rejects.toBe(abortError);
    expect(networkFailure).not.toBeInstanceOf(ApiError);
    expect(abortError).not.toBeInstanceOf(ApiError);
  });

  it("marks malformed meeting and transcript payloads as validation errors", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValueOnce(Response.json({ meetings: [] })),
    );
    await expect(fetchGuildMeetings("guild-1")).rejects.toBeInstanceOf(
      ApiResponseValidationError,
    );

    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValueOnce(Response.json({ segments: "invalid" })),
    );
    await expect(fetchTranscript("meeting-1")).rejects.toMatchObject({
      name: "TranscriptResponseValidationError",
      code: "response_validation",
      message: "文字起こしのレスポンス形式が不正です",
    });
  });
});
