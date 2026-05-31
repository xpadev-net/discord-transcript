import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { buildLoginRedirectUrl } from "./lib/api";

const settingsLinkName = "\u8a2d\u5b9a";
const saveButtonName = "\u4fdd\u5b58";
const forbiddenTitle = "\u8868\u793a\u3067\u304d\u307e\u305b\u3093";
const sessionErrorText =
  "\u6a29\u9650\u60c5\u5831\u3092\u78ba\u8a8d\u3067\u304d\u307e\u305b\u3093\u3067\u3057\u305f";
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
    discord_bot_token_registered: false,
    discord_bot_token_updated_at: null,
    discord_bot_token_last_validated_at: null,
    discord_bot_user_id: null,
    discord_bot_username: null,
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

  it("shows token registration metadata without redisplaying the raw token", async () => {
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
        return Promise.resolve(
          jsonResponse({
            ...settingsResponse(),
            discord_bot_token_registered: true,
            discord_bot_token_updated_at: "2026-05-31T00:01:00Z",
            discord_bot_token_last_validated_at: "2026-05-31T00:01:00Z",
            discord_bot_user_id: "bot-1",
            discord_bot_username: "GuildBot",
          }),
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    expect(await screen.findByText("\u767b\u9332\u6e08\u307f")).toBeTruthy();
    expect(screen.getByText("GuildBot")).toBeTruthy();
    expect(screen.getByText("検証: 2026-05-31T00:01:00Z")).toBeTruthy();
    expect(screen.getByText("更新: 2026-05-31T00:01:00Z")).toBeTruthy();
    const tokenInput = screen.getByLabelText("Bot token") as HTMLInputElement;
    expect(tokenInput.value).toBe("");
    expect(screen.queryByDisplayValue("bot-secret")).toBeNull();
  });

  it("keeps unsaved settings edits when saving a bot token", async () => {
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
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
      if (url === "/api/guild/settings" && !init?.method) {
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      if (url === "/api/guild/settings/bot-token" && init?.method === "PUT") {
        return Promise.resolve(
          jsonResponse({
            ...settingsResponse(),
            discord_bot_token_registered: true,
            discord_bot_token_updated_at: "2026-05-31T00:01:00Z",
            discord_bot_token_last_validated_at: "2026-05-31T00:01:00Z",
            discord_bot_user_id: "bot-1",
            discord_bot_username: "GuildBot",
          }),
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    const autoStopInput = (await screen.findByLabelText(
      "\u81ea\u52d5\u505c\u6b62\u307e\u3067\u306e\u79d2\u6570",
    )) as HTMLInputElement;
    fireEvent.change(autoStopInput, { target: { value: "999" } });
    fireEvent.change(screen.getByLabelText("Bot token"), {
      target: { value: "bot-secret" },
    });
    fireEvent.click(screen.getByRole("button", { name: "\u66f4\u65b0" }));

    await screen.findByText(
      "Discord Bot token \u3092\u4fdd\u5b58\u3057\u307e\u3057\u305f",
    );
    expect(autoStopInput.value).toBe("999");
  });

  it("requires an in-page confirmation before deleting a bot token", async () => {
    let deleteCalls = 0;
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
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
      if (url === "/api/guild/settings" && !init?.method) {
        return Promise.resolve(
          jsonResponse({
            ...settingsResponse(),
            discord_bot_token_registered: true,
            discord_bot_token_updated_at: "2026-05-31T00:01:00Z",
            discord_bot_token_last_validated_at: "2026-05-31T00:01:00Z",
            discord_bot_user_id: "bot-1",
            discord_bot_username: "GuildBot",
          }),
        );
      }
      if (
        url === "/api/guild/settings/bot-token" &&
        init?.method === "DELETE"
      ) {
        deleteCalls += 1;
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    fireEvent.click(
      await screen.findByRole("button", { name: "\u524a\u9664" }),
    );

    expect(deleteCalls).toBe(0);
    expect(
      screen.getByText(
        "Discord Bot token \u306e\u524a\u9664\u3092\u78ba\u8a8d\u3057\u3066\u304f\u3060\u3055\u3044",
      ),
    ).toBeTruthy();

    fireEvent.click(
      screen.getByRole("button", { name: "\u524a\u9664\u3092\u78ba\u5b9a" }),
    );

    await waitFor(() => expect(deleteCalls).toBe(1));
    expect(await screen.findByText("\u672a\u767b\u9332")).toBeTruthy();
  });

  it("clears pending token delete confirmation when saving settings", async () => {
    let deleteCalls = 0;
    let settingsSaveCalls = 0;
    const registeredSettings = {
      ...settingsResponse(),
      discord_bot_token_registered: true,
      discord_bot_token_updated_at: "2026-05-31T00:01:00Z",
      discord_bot_token_last_validated_at: "2026-05-31T00:01:00Z",
      discord_bot_user_id: "bot-1",
      discord_bot_username: "GuildBot",
    };
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
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
      if (url === "/api/guild/settings" && !init?.method) {
        return Promise.resolve(jsonResponse(registeredSettings));
      }
      if (url === "/api/guild/settings" && init?.method === "PUT") {
        settingsSaveCalls += 1;
        return Promise.resolve(jsonResponse(registeredSettings));
      }
      if (
        url === "/api/guild/settings/bot-token" &&
        init?.method === "DELETE"
      ) {
        deleteCalls += 1;
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    fireEvent.click(
      await screen.findByRole("button", { name: "\u524a\u9664" }),
    );
    expect(
      screen.getByRole("button", { name: "\u524a\u9664\u3092\u78ba\u5b9a" }),
    ).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: saveButtonName }));

    await waitFor(() => expect(settingsSaveCalls).toBe(1));
    expect(deleteCalls).toBe(0);
    expect(
      screen.queryByRole("button", { name: "\u524a\u9664\u3092\u78ba\u5b9a" }),
    ).toBeNull();
    expect(screen.getByRole("button", { name: "\u524a\u9664" })).toBeTruthy();
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

  it("shows a session error instead of forbidden when admin status cannot be determined", async () => {
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = input.toString();
      if (url === "/api/me") {
        return Promise.resolve(emptyResponse(500));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    expect(await screen.findByText(sessionErrorText)).toBeTruthy();
    expect(screen.queryByText(forbiddenTitle)).toBeNull();
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
