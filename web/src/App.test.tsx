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
    statusText: statusText(status),
    headers: { "Content-Type": "application/json" },
  });
}

function emptyResponse(status: number): Response {
  return new Response(null, {
    status,
    statusText: statusText(status),
  });
}

function statusText(status: number): string {
  switch (status) {
    case 400:
      return "Bad Request";
    case 401:
      return "Unauthorized";
    case 403:
      return "Forbidden";
    case 404:
      return "Not Found";
    case 409:
      return "Conflict";
    case 500:
      return "Internal Server Error";
    default:
      return "OK";
  }
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

function domainKnowledgeItem(overrides: Record<string, unknown> = {}) {
  return {
    id: "domain-1",
    content_type: "glossary",
    title: "Project Alpha",
    body: "Alpha terms",
    active: true,
    version: 1,
    updated_actor_user_id: "admin-1",
    archived_at: null,
    archived_actor_user_id: null,
    created_at: "2026-06-01T00:00:00.000Z",
    updated_at: "2026-06-01T00:00:00.000Z",
    ...overrides,
  };
}

function summaryTemplate(overrides: Record<string, unknown> = {}) {
  return {
    id: "template-1",
    name: "Default summary",
    template: "Summarize {{ transcript_path }}",
    active: true,
    version: 1,
    updated_actor_user_id: "admin-1",
    archived_at: null,
    archived_actor_user_id: null,
    created_at: "2026-06-01T00:00:00.000Z",
    updated_at: "2026-06-01T00:00:00.000Z",
    ...overrides,
  };
}

function meetingsResponse(
  guildId = "guild-1",
  meetings: unknown[] = [],
  voiceChannels: unknown[] = [],
) {
  return {
    guild_id: guildId,
    meetings,
    voice_channels: voiceChannels,
    page: 1,
    limit: 20,
    total: meetings.length,
  };
}

function meetingItem(overrides: Record<string, unknown> = {}) {
  return {
    id: "meeting-1",
    title: "Meeting One",
    status: "completed",
    started_at: "2026-06-01T00:00:00Z",
    stopped_at: "2026-06-01T00:10:00Z",
    duration_seconds: 600,
    stop_reason: null,
    voice_channel_id: "vc-1",
    ...overrides,
  };
}

function meetingResponse(overrides: Record<string, unknown> = {}) {
  return {
    id: "meeting-1",
    title: "Meeting One",
    status: "posted",
    started_at: "2026-06-01T00:00:00Z",
    stopped_at: "2026-06-01T00:10:00Z",
    duration_seconds: 600,
    ...overrides,
  };
}

function transcriptSegment(overrides: Record<string, unknown> = {}) {
  return {
    id: "segment-1",
    speaker_id: "speaker-1",
    speaker: {
      id: "speaker-1",
      username: "alice",
      nickname: "Alice",
      display_name: "Alice Display",
      display_label: "Alice",
    },
    start_ms: 5000,
    end_ms: 8000,
    text: "Alpha term",
    confidence: null,
    is_noisy: false,
    source: "voice",
    ...overrides,
  };
}

function transcriptResponse(segments: unknown[] = [transcriptSegment()]) {
  return {
    segments,
    status: "posted",
    is_final: true,
    updated_at: "2026-06-01T00:10:00Z",
  };
}

function voiceChannel(id: string, label = `VC ID: ${id}`) {
  return { id, label };
}

function guildsResponse() {
  return [
    {
      guild_id: "guild-1",
      name: "Guild One",
      icon: null,
      is_member: true,
      is_admin: true,
      tenant_id: "tenant-1",
    },
    {
      guild_id: "guild-2",
      name: "Guild Two",
      icon: null,
      is_member: true,
      is_admin: false,
      tenant_id: "tenant-2",
    },
    {
      guild_id: "guild-3",
      name: "Guild Three",
      icon: null,
      is_member: true,
      is_admin: true,
      tenant_id: null,
    },
  ];
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function renderApp(route: string, fetchMock: ReturnType<typeof vi.fn>) {
  vi.stubGlobal("fetch", fetchMock);
  return render(
    <MemoryRouter initialEntries={[route]}>
      <App />
    </MemoryRouter>,
  );
}

function meetingPageFetch(options: { feedbackStatus?: number } = {}) {
  let feedbackRequest: unknown = null;
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
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
    if (url === "/api/me/guilds") {
      return Promise.resolve(jsonResponse(guildsResponse().slice(0, 1)));
    }
    if (url === "/api/meetings/meeting-1") {
      return Promise.resolve(jsonResponse(meetingResponse()));
    }
    if (url === "/api/meetings/meeting-1/transcript") {
      return Promise.resolve(jsonResponse(transcriptResponse()));
    }
    if (url === "/api/meetings/meeting-1/summary") {
      return Promise.resolve(jsonResponse({ markdown: null }));
    }
    if (url === "/api/meetings/meeting-1/debug/manifest") {
      return Promise.resolve(jsonResponse([]));
    }
    if (url === "/api/meetings/meeting-1/feedback" && init?.method === "POST") {
      feedbackRequest = JSON.parse(String(init.body));
      const status = options.feedbackStatus ?? 201;
      if (status >= 400) {
        return Promise.resolve(emptyResponse(status));
      }
      return Promise.resolve(
        jsonResponse(
          {
            id: "feedback-1",
            meeting_id: "meeting-1",
            transcript_segment_id: "segment-1",
            feedback_type: "mistranscription",
            term_type: null,
            original_text: "Alpha term",
            corrected_text: "Alpha team",
            speaker_id: null,
            corrected_speaker_id: null,
            note: null,
            actor_user_id: "member-1",
            status: "open",
            created_at: "2026-06-01T00:11:00Z",
            reviewed_at: null,
            reviewed_actor_user_id: null,
          },
          201,
        ),
      );
    }
    return Promise.resolve(emptyResponse(404));
  });

  return {
    fetchMock,
    feedbackRequest: () => feedbackRequest,
  };
}

afterEach(() => {
  cleanup();
  window.localStorage.clear();
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

  it("loads active domain knowledge and summary template versions for admins", async () => {
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
      if (url === "/api/guild/domain-knowledge?include_archived=true") {
        return Promise.resolve(jsonResponse([domainKnowledgeItem()]));
      }
      if (url === "/api/guild/summary-templates?include_archived=true") {
        return Promise.resolve(jsonResponse([summaryTemplate()]));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    expect(await screen.findByText("AI カスタマイズ")).toBeTruthy();
    expect(screen.getByText("有効: Project Alpha v1")).toBeTruthy();
    expect(screen.getByText("有効: Default summary v1")).toBeTruthy();
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/guild/domain-knowledge?include_archived=true",
        expect.anything(),
      ),
    );
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/guild/summary-templates?include_archived=true",
      expect.anything(),
    );
  });

  it("saves edited domain knowledge drafts with trimmed fields", async () => {
    let savedRequest: unknown = null;
    let currentDomain = domainKnowledgeItem();
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
      if (url === "/api/guild/domain-knowledge?include_archived=true") {
        return Promise.resolve(jsonResponse([currentDomain]));
      }
      if (url === "/api/guild/summary-templates?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      if (
        url === "/api/guild/domain-knowledge/domain-1" &&
        init?.method === "PUT"
      ) {
        savedRequest = JSON.parse(String(init.body));
        currentDomain = domainKnowledgeItem({
          title: "Beta Term",
          body: "Beta body",
          version: 2,
          updated_at: "2026-06-02T00:00:00.000Z",
        });
        return Promise.resolve(jsonResponse(currentDomain));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    fireEvent.change(await screen.findByLabelText("タイトル"), {
      target: { value: "  Beta Term  " },
    });
    fireEvent.change(screen.getByLabelText("本文"), {
      target: { value: "  Beta body  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "ドメイン知識を保存" }));

    await waitFor(() =>
      expect(savedRequest).toEqual({
        content_type: "glossary",
        title: "Beta Term",
        body: "Beta body",
        active: true,
      }),
    );
    expect(await screen.findByText("有効: Beta Term v2")).toBeTruthy();
  });

  it("activates a summary template and refreshes the active version", async () => {
    let activateCalls = 0;
    let templates = [
      summaryTemplate(),
      summaryTemplate({
        id: "template-2",
        name: "Focus summary",
        template: "Focus {{ speaker_roster }}",
        active: false,
        version: 1,
      }),
    ];
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
      if (url === "/api/guild/domain-knowledge?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      if (url === "/api/guild/summary-templates?include_archived=true") {
        return Promise.resolve(jsonResponse(templates));
      }
      if (
        url === "/api/guild/summary-templates/template-2/activate" &&
        init?.method === "POST"
      ) {
        activateCalls += 1;
        templates = [
          summaryTemplate({ active: false, version: 2 }),
          summaryTemplate({
            id: "template-2",
            name: "Focus summary",
            template: "Focus {{ speaker_roster }}",
            active: true,
            version: 2,
          }),
        ];
        return Promise.resolve(jsonResponse(templates[1]));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    const versionSelects = (await screen.findAllByLabelText(
      "バージョン",
    )) as HTMLSelectElement[];
    fireEvent.change(versionSelects[1], {
      target: { value: "template-2" },
    });
    const activateButton = screen
      .getAllByRole("button", { name: "有効化" })
      .find((button) => !(button as HTMLButtonElement).disabled);
    expect(activateButton).toBeTruthy();
    fireEvent.click(activateButton as HTMLButtonElement);

    await waitFor(() => expect(activateCalls).toBe(1));
    expect(await screen.findByText("有効: Focus summary v2")).toBeTruthy();
  });

  it("shows inline validation errors without calling summary template APIs", async () => {
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
      if (url === "/api/guild/domain-knowledge?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      if (url === "/api/guild/summary-templates?include_archived=true") {
        return Promise.resolve(jsonResponse([summaryTemplate()]));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    fireEvent.change(await screen.findByLabelText("テンプレート"), {
      target: { value: "Summarize {{ unknown_variable }}" },
    });
    const callsBeforeSave = fetchMock.mock.calls.length;
    fireEvent.click(
      screen.getByRole("button", { name: "要約テンプレートを保存" }),
    );

    expect(
      await screen.findByText(
        "使用できない要約テンプレート変数です: unknown_variable",
      ),
    ).toBeTruthy();
    expect(fetchMock.mock.calls.length).toBe(callsBeforeSave);
  });

  it("validates domain knowledge title byte length before saving", async () => {
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
      if (url === "/api/guild/domain-knowledge?include_archived=true") {
        return Promise.resolve(jsonResponse([domainKnowledgeItem()]));
      }
      if (url === "/api/guild/summary-templates?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    fireEvent.change(await screen.findByLabelText("タイトル"), {
      target: { value: "議".repeat(80) },
    });
    const callsBeforeSave = fetchMock.mock.calls.length;
    fireEvent.click(screen.getByRole("button", { name: "ドメイン知識を保存" }));

    expect(
      await screen.findByText(
        "ドメイン知識のタイトルはUTF-8で200バイト以内で入力してください",
      ),
    ).toBeTruthy();
    expect(fetchMock.mock.calls.length).toBe(callsBeforeSave);
  });

  it("shows server validation errors inline for domain knowledge saves", async () => {
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
      if (url === "/api/guild/domain-knowledge?include_archived=true") {
        return Promise.resolve(jsonResponse([domainKnowledgeItem()]));
      }
      if (url === "/api/guild/summary-templates?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      if (
        url === "/api/guild/domain-knowledge/domain-1" &&
        init?.method === "PUT"
      ) {
        return Promise.resolve(emptyResponse(400));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/settings", fetchMock);

    fireEvent.change(await screen.findByLabelText("本文"), {
      target: { value: "Changed body" },
    });
    fireEvent.click(screen.getByRole("button", { name: "ドメイン知識を保存" }));

    expect(
      await screen.findByText("入力内容がサーバーの検証に通りませんでした"),
    ).toBeTruthy();
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
    expect(fetchMock).not.toHaveBeenCalledWith(
      "/api/guild/domain-knowledge?include_archived=true",
      expect.anything(),
    );
    expect(fetchMock).not.toHaveBeenCalledWith(
      "/api/guild/summary-templates?include_archived=true",
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
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        throw new Error(
          "selector-unavailable fallback should use current-guild route",
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(
      (await screen.findAllByText(emptyMeetingsText)).length,
    ).toBeGreaterThan(0);
    expect(screen.getByRole("table")).toBeTruthy();
  });

  it("renders the guild selector with installed guilds selectable", async () => {
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(jsonResponse(guildsResponse()));
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        return Promise.resolve(jsonResponse(meetingsResponse()));
      }
      if (url.startsWith("/api/guilds/guild-2/meetings")) {
        return Promise.resolve(jsonResponse(meetingsResponse("guild-2")));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    const selector = (await screen.findByRole("combobox", {
      name: "\u30ae\u30eb\u30c9",
    })) as HTMLSelectElement;
    expect(selector.value).toBe("guild-1");
    expect(
      await screen.findByRole("link", { name: settingsLinkName }),
    ).toBeTruthy();

    const uninstalled = screen.getByRole("option", {
      name: "Guild Three（未導入）",
    }) as HTMLOptionElement;
    expect(uninstalled.disabled).toBe(true);

    fireEvent.change(selector, { target: { value: "guild-2" } });

    expect(selector.value).toBe("guild-2");
    expect(window.localStorage.getItem("dt.selectedGuildId")).toBe("guild-2");
    expect(screen.queryByRole("link", { name: settingsLinkName })).toBeNull();
  });

  it("loads dashboard meetings for the stored selected guild", async () => {
    window.localStorage.setItem("dt.selectedGuildId", "guild-2");
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(jsonResponse(guildsResponse()));
      }
      if (url.startsWith("/api/guilds/guild-2/meetings")) {
        return Promise.resolve(
          jsonResponse(
            meetingsResponse("guild-2", [
              {
                id: "meeting-2",
                title: "Guild Two meeting",
                status: "completed",
                started_at: "2026-06-01T00:00:00Z",
                stopped_at: "2026-06-01T00:10:00Z",
                duration_seconds: 600,
                stop_reason: null,
              },
            ]),
          ),
        );
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        throw new Error("stale current-guild dashboard request");
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(await screen.findByText("Guild Two meeting")).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/guilds/guild-2/meetings?page=1&limit=20",
      expect.anything(),
    );
  });

  it("filters dashboard meetings by VC and clears the VC filter", async () => {
    const channels = [voiceChannel("vc-1"), voiceChannel("vc-2")];
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(jsonResponse(guildsResponse()));
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        const params = new URL(`http://localhost${url}`).searchParams;
        if (params.get("voice_channel_id") === "vc-2") {
          return Promise.resolve(
            jsonResponse(
              meetingsResponse(
                "guild-1",
                [
                  meetingItem({
                    id: "meeting-2",
                    title: "VC Two meeting",
                    voice_channel_id: "vc-2",
                  }),
                ],
                channels,
              ),
            ),
          );
        }
        return Promise.resolve(
          jsonResponse(
            meetingsResponse(
              "guild-1",
              [
                meetingItem({
                  id: "meeting-1",
                  title: "VC One meeting",
                  voice_channel_id: "vc-1",
                }),
                meetingItem({
                  id: "meeting-2",
                  title: "VC Two meeting",
                  voice_channel_id: "vc-2",
                }),
              ],
              channels,
            ),
          ),
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(await screen.findByText("VC One meeting")).toBeTruthy();
    const vcSelector = (await screen.findByRole("combobox", {
      name: "VC",
    })) as HTMLSelectElement;
    fireEvent.change(vcSelector, { target: { value: "vc-2" } });

    expect(await screen.findByText("VC Two meeting")).toBeTruthy();
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/guilds/guild-1/meetings?page=1&limit=20&voice_channel_id=vc-2",
        expect.anything(),
      ),
    );

    fireEvent.change(vcSelector, { target: { value: "" } });

    await waitFor(() => {
      const unfilteredCalls = fetchMock.mock.calls.filter(
        ([calledUrl]) =>
          calledUrl.toString() ===
          "/api/guilds/guild-1/meetings?page=1&limit=20",
      );
      expect(unfilteredCalls.length).toBeGreaterThanOrEqual(2);
    });
  });

  it("shows an empty state when the selected VC has no meetings", async () => {
    const channels = [voiceChannel("vc-1"), voiceChannel("vc-empty")];
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(jsonResponse(guildsResponse()));
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        const params = new URL(`http://localhost${url}`).searchParams;
        if (params.get("voice_channel_id") === "vc-empty") {
          return Promise.resolve(
            jsonResponse(meetingsResponse("guild-1", [], [])),
          );
        }
        return Promise.resolve(
          jsonResponse(
            meetingsResponse(
              "guild-1",
              [
                meetingItem({
                  title: "VC One meeting",
                  voice_channel_id: "vc-1",
                }),
              ],
              channels,
            ),
          ),
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(await screen.findByText("VC One meeting")).toBeTruthy();
    const vcSelector = (await screen.findByRole("combobox", {
      name: "VC",
    })) as HTMLSelectElement;
    fireEvent.change(vcSelector, { target: { value: "vc-empty" } });

    expect(
      (await screen.findAllByText("このVCの会議はありません")).length,
    ).toBeGreaterThan(0);
    expect(vcSelector.disabled).toBe(false);

    fireEvent.change(vcSelector, { target: { value: "" } });

    await waitFor(() => {
      const unfilteredCalls = fetchMock.mock.calls.filter(
        ([calledUrl]) =>
          calledUrl.toString() ===
          "/api/guilds/guild-1/meetings?page=1&limit=20",
      );
      expect(unfilteredCalls.length).toBeGreaterThanOrEqual(2);
    });
  });

  it("clears the selected VC when switching guilds", async () => {
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(jsonResponse(guildsResponse()));
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        return Promise.resolve(
          jsonResponse(
            meetingsResponse(
              "guild-1",
              [
                meetingItem({
                  title: "Guild One meeting",
                  voice_channel_id: "vc-1",
                }),
              ],
              [voiceChannel("vc-1")],
            ),
          ),
        );
      }
      if (url.startsWith("/api/guilds/guild-2/meetings")) {
        return Promise.resolve(
          jsonResponse(
            meetingsResponse(
              "guild-2",
              [
                meetingItem({
                  id: "meeting-2",
                  title: "Guild Two meeting",
                  voice_channel_id: "vc-9",
                }),
              ],
              [voiceChannel("vc-9")],
            ),
          ),
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(await screen.findByText("Guild One meeting")).toBeTruthy();
    const vcSelector = (await screen.findByRole("combobox", {
      name: "VC",
    })) as HTMLSelectElement;
    fireEvent.change(vcSelector, { target: { value: "vc-1" } });

    const guildSelector = (await screen.findByRole("combobox", {
      name: "\u30ae\u30eb\u30c9",
    })) as HTMLSelectElement;
    fireEvent.change(guildSelector, { target: { value: "guild-2" } });

    expect(await screen.findByText("Guild Two meeting")).toBeTruthy();
    const guildTwoCalls = fetchMock.mock.calls
      .map(([calledUrl]) => calledUrl.toString())
      .filter((calledUrl) =>
        calledUrl.startsWith("/api/guilds/guild-2/meetings"),
      );
    expect(guildTwoCalls.length).toBeGreaterThan(0);
    expect(
      guildTwoCalls.every(
        (calledUrl) => !calledUrl.includes("voice_channel_id"),
      ),
    ).toBe(true);
  });

  it("does not display stale meetings after switching guilds", async () => {
    const staleGuildOne = deferred<Response>();
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(jsonResponse(guildsResponse()));
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        return staleGuildOne.promise;
      }
      if (url.startsWith("/api/guilds/guild-2/meetings")) {
        return Promise.resolve(
          jsonResponse(
            meetingsResponse("guild-2", [
              {
                id: "meeting-2",
                title: "Guild Two meeting",
                status: "completed",
                started_at: "2026-06-01T00:00:00Z",
                stopped_at: "2026-06-01T00:10:00Z",
                duration_seconds: 600,
                stop_reason: null,
              },
            ]),
          ),
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    const selector = (await screen.findByRole("combobox", {
      name: "\u30ae\u30eb\u30c9",
    })) as HTMLSelectElement;
    fireEvent.change(selector, { target: { value: "guild-2" } });
    expect(await screen.findByText("Guild Two meeting")).toBeTruthy();

    staleGuildOne.resolve(
      jsonResponse(
        meetingsResponse("guild-1", [
          {
            id: "meeting-1",
            title: "Guild One stale meeting",
            status: "completed",
            started_at: "2026-06-01T00:00:00Z",
            stopped_at: "2026-06-01T00:10:00Z",
            duration_seconds: 600,
            stop_reason: null,
          },
        ]),
      ),
    );

    await waitFor(() =>
      expect(screen.queryByText("Guild One stale meeting")).toBeNull(),
    );
  });

  it("loads guild-targeted settings and displays the target guild name", async () => {
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(
          jsonResponse([
            ...guildsResponse().slice(0, 1),
            {
              guild_id: "guild-2",
              name: "Guild Two",
              icon: null,
              is_member: true,
              is_admin: true,
              tenant_id: "tenant-2",
            },
          ]),
        );
      }
      if (url === "/api/guilds/guild-2/settings") {
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/guilds/guild-2/settings", fetchMock);

    expect(await screen.findByText("Guild Two のギルド設定")).toBeTruthy();
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/guilds/guild-2/settings",
        expect.anything(),
      ),
    );
    expect(fetchMock).not.toHaveBeenCalledWith(
      "/api/guild/settings",
      expect.anything(),
    );
  });

  it("saves guild-targeted settings to the route guild", async () => {
    let savedUrl: string | null = null;
    let savedRequest: unknown = null;
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(
          jsonResponse([
            ...guildsResponse().slice(0, 1),
            {
              guild_id: "guild-2",
              name: "Guild Two",
              icon: null,
              is_member: true,
              is_admin: true,
              tenant_id: "tenant-2",
            },
          ]),
        );
      }
      if (url === "/api/guilds/guild-2/settings" && !init?.method) {
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      if (url === "/api/guilds/guild-2/settings" && init?.method === "PUT") {
        savedUrl = url;
        savedRequest = JSON.parse(String(init.body));
        return Promise.resolve(
          jsonResponse({
            ...settingsResponse(),
            auto_stop_grace_seconds: 240,
          }),
        );
      }
      if (url === "/api/guild/settings" && init?.method === "PUT") {
        throw new Error("stale current-guild save");
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/guilds/guild-2/settings", fetchMock);

    const autoStopInput = (await screen.findByLabelText(
      "\u81ea\u52d5\u505c\u6b62\u307e\u3067\u306e\u79d2\u6570",
    )) as HTMLInputElement;
    fireEvent.change(autoStopInput, { target: { value: "240" } });
    fireEvent.click(screen.getByRole("button", { name: saveButtonName }));

    await waitFor(() => expect(savedUrl).toBe("/api/guilds/guild-2/settings"));
    expect(savedRequest).toMatchObject({
      auto_stop_grace_seconds: 240,
    });
  });

  it("ignores stale save responses after switching target guilds", async () => {
    const staleSave = deferred<Response>();
    let guildTwoSaveRequest: unknown = null;
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(
          jsonResponse([
            {
              guild_id: "guild-1",
              name: "Guild One",
              icon: null,
              is_member: true,
              is_admin: true,
              tenant_id: "tenant-1",
            },
            {
              guild_id: "guild-2",
              name: "Guild Two",
              icon: null,
              is_member: true,
              is_admin: true,
              tenant_id: "tenant-2",
            },
          ]),
        );
      }
      if (url === "/api/guilds/guild-1/settings" && !init?.method) {
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      if (url === "/api/guilds/guild-1/settings" && init?.method === "PUT") {
        return staleSave.promise;
      }
      if (url === "/api/guilds/guild-2/settings" && !init?.method) {
        return Promise.resolve(
          jsonResponse({
            ...settingsResponse(),
            auto_stop_grace_seconds: 240,
          }),
        );
      }
      if (url === "/api/guilds/guild-2/settings" && init?.method === "PUT") {
        guildTwoSaveRequest = JSON.parse(String(init.body));
        return Promise.resolve(
          jsonResponse({
            ...settingsResponse(),
            auto_stop_grace_seconds: 240,
          }),
        );
      }
      if (url === "/api/guild/domain-knowledge?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      if (url === "/api/guild/summary-templates?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/guilds/guild-1/settings", fetchMock);

    const autoStopInput = (await screen.findByLabelText(
      "\u81ea\u52d5\u505c\u6b62\u307e\u3067\u306e\u79d2\u6570",
    )) as HTMLInputElement;
    fireEvent.change(autoStopInput, { target: { value: "333" } });
    fireEvent.click(screen.getByRole("button", { name: saveButtonName }));

    const selector = screen.getByRole("combobox", {
      name: "\u30ae\u30eb\u30c9",
    }) as HTMLSelectElement;
    fireEvent.change(selector, { target: { value: "guild-2" } });

    expect(await screen.findByText("Guild Two のギルド設定")).toBeTruthy();
    const guildTwoAutoStopInput = screen.getByLabelText(
      "\u81ea\u52d5\u505c\u6b62\u307e\u3067\u306e\u79d2\u6570",
    ) as HTMLInputElement;
    await waitFor(() => expect(guildTwoAutoStopInput.value).toBe("240"));

    staleSave.resolve(
      jsonResponse({
        ...settingsResponse(),
        auto_stop_grace_seconds: 333,
      }),
    );

    await waitFor(() => expect(guildTwoAutoStopInput.value).toBe("240"));
    fireEvent.click(screen.getByRole("button", { name: saveButtonName }));

    await waitFor(() =>
      expect(guildTwoSaveRequest).toMatchObject({
        auto_stop_grace_seconds: 240,
      }),
    );
  });

  it("moves an open settings page to the newly selected guild", async () => {
    let savedUrl: string | null = null;
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(
          jsonResponse([
            {
              guild_id: "guild-1",
              name: "Guild One",
              icon: null,
              is_member: true,
              is_admin: true,
              tenant_id: "tenant-1",
            },
            {
              guild_id: "guild-2",
              name: "Guild Two",
              icon: null,
              is_member: true,
              is_admin: true,
              tenant_id: "tenant-2",
            },
          ]),
        );
      }
      if (url === "/api/guilds/guild-1/settings") {
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      if (url === "/api/guilds/guild-2/settings" && !init?.method) {
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      if (url === "/api/guilds/guild-2/settings" && init?.method === "PUT") {
        savedUrl = url;
        return Promise.resolve(jsonResponse(settingsResponse()));
      }
      if (url === "/api/guild/domain-knowledge?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      if (url === "/api/guild/summary-templates?include_archived=true") {
        return Promise.resolve(jsonResponse([]));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/guilds/guild-1/settings", fetchMock);

    const selector = (await screen.findByRole("combobox", {
      name: "\u30ae\u30eb\u30c9",
    })) as HTMLSelectElement;
    fireEvent.change(selector, { target: { value: "guild-2" } });

    expect(await screen.findByText("Guild Two のギルド設定")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: saveButtonName }));

    await waitFor(() => expect(savedUrl).toBe("/api/guilds/guild-2/settings"));
  });

  it("blocks a guild-targeted settings route for non-admin selected guilds", async () => {
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(jsonResponse(guildsResponse()));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/guilds/guild-2/settings", fetchMock);

    expect(await screen.findByText(forbiddenTitle)).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalledWith(
      "/api/guilds/guild-2/settings",
      expect.anything(),
    );
  });

  it("shows a no-selectable-guild state for guild-targeted settings", async () => {
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(
          jsonResponse([
            {
              guild_id: "guild-3",
              name: "Guild Three",
              icon: null,
              is_member: true,
              is_admin: true,
              tenant_id: null,
            },
          ]),
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/guilds/guild-3/settings", fetchMock);

    expect(
      await screen.findByText("設定できるギルドがありません"),
    ).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalledWith(
      "/api/guilds/guild-3/settings",
      expect.anything(),
    );
  });

  it("falls back to the authenticated guild when guild selector data is unavailable", async () => {
    window.localStorage.setItem("dt.selectedGuildId", "guild-2");
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(emptyResponse(503));
      }
      if (url.startsWith("/api/guild/meetings")) {
        return Promise.resolve(jsonResponse(meetingsResponse()));
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        throw new Error(
          "selector-unavailable fallback should use current-guild route",
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(
      (await screen.findAllByText(emptyMeetingsText)).length,
    ).toBeGreaterThan(0);
    expect(
      screen.queryByRole("combobox", { name: "\u30ae\u30eb\u30c9" }),
    ).toBeNull();
    await waitFor(() =>
      expect(window.localStorage.getItem("dt.selectedGuildId")).toBe("guild-1"),
    );
    expect(screen.getByRole("link", { name: settingsLinkName })).toBeTruthy();
  });

  it("keeps VC filtering on the current-guild fallback meetings route", async () => {
    const channels = [voiceChannel("vc-1"), voiceChannel("vc-2")];
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(emptyResponse(503));
      }
      if (url.startsWith("/api/guild/meetings")) {
        const params = new URL(`http://localhost${url}`).searchParams;
        if (params.get("voice_channel_id") === "vc-2") {
          return Promise.resolve(
            jsonResponse(
              meetingsResponse(
                "guild-1",
                [
                  meetingItem({
                    id: "meeting-2",
                    title: "Fallback VC Two",
                    voice_channel_id: "vc-2",
                  }),
                ],
                channels,
              ),
            ),
          );
        }
        return Promise.resolve(
          jsonResponse(
            meetingsResponse(
              "guild-1",
              [
                meetingItem({
                  title: "Fallback VC One",
                  voice_channel_id: "vc-1",
                }),
              ],
              channels,
            ),
          ),
        );
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        throw new Error(
          "selector-unavailable fallback should use current-guild route",
        );
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(await screen.findByText("Fallback VC One")).toBeTruthy();
    const vcSelector = (await screen.findByRole("combobox", {
      name: "VC",
    })) as HTMLSelectElement;
    fireEvent.change(vcSelector, { target: { value: "vc-2" } });

    expect(await screen.findByText("Fallback VC Two")).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/guild/meetings?page=1&limit=20&voice_channel_id=vc-2",
      expect.anything(),
    );
  });

  it("shows a no-guild dashboard state when visible guilds are not installed", async () => {
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(
          jsonResponse([
            {
              guild_id: "guild-3",
              name: "Guild Three",
              icon: null,
              is_member: true,
              is_admin: true,
              tenant_id: null,
            },
          ]),
        );
      }
      if (url.includes("/meetings")) {
        throw new Error("uninstalled guild should not load meetings");
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(
      (await screen.findAllByText("表示できるギルドがありません")).length,
    ).toBeGreaterThan(0);
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("does not render dashboard table data when membership is denied", async () => {
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
      if (url === "/api/me/guilds") {
        return Promise.resolve(jsonResponse(guildsResponse().slice(0, 1)));
      }
      if (url.startsWith("/api/guilds/guild-1/meetings")) {
        return Promise.resolve(emptyResponse(403));
      }
      return Promise.resolve(emptyResponse(404));
    });

    renderApp("/", fetchMock);

    expect(await screen.findByText(dashboardForbiddenText)).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("opens transcript feedback without consuming row seek clicks", async () => {
    const { fetchMock } = meetingPageFetch();
    renderApp("/meetings/meeting-1", fetchMock);

    expect(await screen.findByText("Alpha term")).toBeTruthy();

    fireEvent.click(
      screen.getByRole("button", { name: "00:05 のフィードバック" }),
    );

    expect(screen.getByRole("dialog", { name: "フィードバック" })).toBeTruthy();
    expect(screen.queryByText("音声の読み込みが完了していません")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "キャンセル" }));
    const segmentButton = screen.getByText("Alpha term").closest("button");
    expect(segmentButton).toBeTruthy();
    fireEvent.click(segmentButton as HTMLButtonElement);

    expect(
      await screen.findByText("音声の読み込みが完了していません"),
    ).toBeTruthy();
  });

  it("submits corrected transcript feedback to the meeting API", async () => {
    const { fetchMock, feedbackRequest } = meetingPageFetch();
    renderApp("/meetings/meeting-1", fetchMock);

    fireEvent.click(
      await screen.findByRole("button", {
        name: "00:05 のフィードバック",
      }),
    );
    fireEvent.change(screen.getByLabelText("修正後の文字起こし"), {
      target: { value: "Alpha team" },
    });
    fireEvent.change(screen.getByLabelText("メモ・ヒント"), {
      target: { value: "team name" },
    });
    fireEvent.click(screen.getByRole("button", { name: "送信" }));

    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/meetings/meeting-1/feedback",
        expect.objectContaining({ method: "POST" }),
      ),
    );
    expect(feedbackRequest()).toEqual({
      transcript_segment_id: "segment-1",
      feedback_type: "mistranscription",
      original_text: "Alpha term",
      corrected_text: "Alpha team",
      note: "team name",
    });
    expect(
      await screen.findByText("フィードバックを送信しました"),
    ).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "フィードバック" })).toBeNull();
  });

  it("keeps feedback open and reports API validation errors", async () => {
    const { fetchMock } = meetingPageFetch({ feedbackStatus: 400 });
    renderApp("/meetings/meeting-1", fetchMock);

    fireEvent.click(
      await screen.findByRole("button", {
        name: "00:05 のフィードバック",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "送信" }));

    expect(
      await screen.findByText("入力内容がサーバーの検証に通りませんでした"),
    ).toBeTruthy();
    expect(screen.getByRole("dialog", { name: "フィードバック" })).toBeTruthy();
  });

  it("exposes speaker and term feedback controls accessibly", async () => {
    const { fetchMock } = meetingPageFetch();
    renderApp("/meetings/meeting-1", fetchMock);

    const feedbackButton = await screen.findByRole("button", {
      name: "00:05 のフィードバック",
    });
    fireEvent.click(feedbackButton);
    const dialog = screen.getByRole("dialog", { name: "フィードバック" });
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(screen.getByLabelText("種類")).toBe(document.activeElement);

    fireEvent.change(screen.getByLabelText("種類"), {
      target: { value: "speaker" },
    });
    expect(screen.getByLabelText("正しい話者IDまたは名前")).toBeTruthy();
    expect(screen.queryByLabelText("修正後の文字起こし")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "送信" }));
    expect(
      await screen.findByText("正しい話者IDまたは名前を入力してください"),
    ).toBeTruthy();

    fireEvent.change(screen.getByLabelText("種類"), {
      target: { value: "term" },
    });
    expect(screen.getByLabelText("用語タイプ")).toBeTruthy();

    const submitButton = screen.getByRole("button", { name: "送信" });
    submitButton.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(screen.getByLabelText("種類")).toBe(document.activeElement);

    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "フィードバック" }),
      ).toBeNull(),
    );
    await waitFor(() => expect(feedbackButton).toBe(document.activeElement));
  });

  it("submits speaker correction feedback without a dropped text field", async () => {
    const { fetchMock, feedbackRequest } = meetingPageFetch();
    renderApp("/meetings/meeting-1", fetchMock);

    fireEvent.click(
      await screen.findByRole("button", {
        name: "00:05 のフィードバック",
      }),
    );
    fireEvent.change(screen.getByLabelText("種類"), {
      target: { value: "speaker" },
    });
    fireEvent.change(screen.getByLabelText("正しい話者IDまたは名前"), {
      target: { value: "speaker-2" },
    });
    fireEvent.change(screen.getByLabelText("メモ・ヒント"), {
      target: { value: "Bob was speaking" },
    });
    fireEvent.click(screen.getByRole("button", { name: "送信" }));

    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/meetings/meeting-1/feedback",
        expect.objectContaining({ method: "POST" }),
      ),
    );
    expect(feedbackRequest()).toEqual({
      transcript_segment_id: "segment-1",
      feedback_type: "speaker",
      original_text: "Alpha term",
      speaker_id: "speaker-1",
      corrected_speaker_id: "speaker-2",
      note: "Bob was speaking",
    });
  });

  it("builds the expired-session login redirect with the current route preserved", () => {
    expect(buildLoginRedirectUrl("/settings?tab=a#section")).toBe(
      "/auth/login?redirect=%2Fsettings%3Ftab%3Da%23section",
    );
  });
});
