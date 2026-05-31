import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { buildLoginRedirectUrl } from "./lib/api";

const settingsLinkName = "\u8a2d\u5b9a";
const saveButtonName = "\u4fdd\u5b58";
const forbiddenTitle = "\u8868\u793a\u3067\u304d\u307e\u305b\u3093";
const emptyMeetingsText =
  "\u4f1a\u8b70\u306f\u307e\u3060\u3042\u308a\u307e\u305b\u3093";
const dashboardForbiddenText =
  "\u3053\u306e\u30ae\u30eb\u30c9\u306e\u4f1a\u8b70\u3092\u8868\u793a\u3059\u308b\u6a29\u9650\u304c\u3042\u308a\u307e\u305b\u3093";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    statusText: status === 403 ? "Forbidden" : "OK",
    headers: { "Content-Type": "application/json" },
  });
}

function emptyResponse(status: number): Response {
  return new Response(null, {
    status,
    statusText: status === 403 ? "Forbidden" : "Unauthorized",
  });
}

function settingsResponse() {
  return {
    whisper_language: "ja",
    whisper_language_explicit: true,
    whisper_vad: false,
    auto_stop_grace_seconds: 120,
    retention_raw_audio_ttl_days: 14,
    retention_transcript_ttl_days: 60,
    summary_enabled: true,
    is_admin: true,
  };
}

function meetingsResponse() {
  return {
    meetings: [],
    page: 1,
    limit: 20,
    total: 0,
  };
}

function renderApp(route: string, fetchMock: ReturnType<typeof vi.fn>) {
  vi.stubGlobal("fetch", fetchMock);
  return render(
    <MemoryRouter initialEntries={[route]}>
      <App />
    </MemoryRouter>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("App access controls", () => {
  it("shows settings navigation and controls for guild admins", async () => {
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = input.toString();
      if (url === "/api/me") {
        return Promise.resolve(
          jsonResponse({
            user_id: "admin-1",
            guild_id: "guild-1",
            is_admin: true,
          }),
        );
      }
      if (url === "/api/guild/settings") {
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    expect(
      await screen.findByRole("link", { name: settingsLinkName }),
    ).toBeTruthy();
    expect(
      await screen.findByRole("button", { name: saveButtonName }),
    ).toBeTruthy();
    expect(screen.queryByText(forbiddenTitle)).toBeNull();
  });

  it("hides settings navigation and blocks direct settings access for non-admin members", async () => {
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = input.toString();
      if (url === "/api/me") {
        return Promise.resolve(
          jsonResponse({
            user_id: "member-1",
            guild_id: "guild-1",
            is_admin: false,
          }),
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    expect(await screen.findByText(forbiddenTitle)).toBeTruthy();
    expect(screen.queryByRole("link", { name: settingsLinkName })).toBeNull();
    expect(screen.queryByRole("button", { name: saveButtonName })).toBeNull();
    expect(fetchMock).not.toHaveBeenCalledWith(
      "/api/guild/settings",
      expect.anything(),
    );
  });

  it("renders dashboard data for guild members", async () => {
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = input.toString();
      if (url === "/api/me") {
        return Promise.resolve(
          jsonResponse({
            user_id: "member-1",
            guild_id: "guild-1",
            is_admin: false,
          }),
        );
      }
      if (url.startsWith("/api/guild/meetings")) {
        return Promise.resolve(jsonResponse(meetingsResponse()));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(
      (await screen.findAllByText(emptyMeetingsText)).length,
    ).toBeGreaterThan(0);
    expect(screen.getByRole("table")).toBeTruthy();
  });

  it("does not render dashboard table data when membership is denied", async () => {
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = input.toString();
      if (url === "/api/me" || url.startsWith("/api/guild/meetings")) {
        return Promise.resolve(emptyResponse(403));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(await screen.findByText(dashboardForbiddenText)).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("builds the expired-session login redirect with the current route preserved", () => {
    expect(buildLoginRedirectUrl("/settings?tab=a#section")).toBe(
      "/auth/login?redirect=%2Fsettings%3Ftab%3Da%23section",
    );
  });
});
