import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fetchMeeting, fetchSummary, fetchTranscript } from "../lib/api";
import type {
  MeetingResponse,
  SummaryResponse,
  TranscriptResponse,
} from "../lib/types";
import { useMeetingData } from "./useMeetingData";

vi.mock("../lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/api")>();
  return {
    ...actual,
    fetchMeeting: vi.fn(),
    fetchSummary: vi.fn(),
    fetchTranscript: vi.fn(),
  };
});

class TestEventSource {
  static latest: TestEventSource | null = null;
  static instances: TestEventSource[] = [];
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  close = vi.fn();
  private listeners = new Map<
    string,
    Array<(event: MessageEvent<string>) => void>
  >();

  constructor(_url: string, _options?: EventSourceInit) {
    TestEventSource.latest = this;
    TestEventSource.instances.push(this);
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject) {
    if (typeof listener !== "function") {
      return;
    }
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener as (event: MessageEvent<string>) => void);
    this.listeners.set(type, listeners);
  }

  emit(type: string, data: string) {
    for (const listener of this.listeners.get(type) ?? []) {
      listener(new MessageEvent(type, { data }));
    }
  }
}

function meeting(id: string, status: string): MeetingResponse {
  return {
    id,
    title: id,
    status,
    started_at: "2026-06-01T00:00:00Z",
    stopped_at: null,
    duration_seconds: null,
    voice_channel_id: "vc-1",
    voice_channel_name: null,
  };
}

function transcript(status: string, isFinal: boolean): TranscriptResponse {
  return {
    segments: [],
    status,
    is_final: isFinal,
    reset: false,
    updated_at: null,
  };
}

function segment(
  id: string,
  source: "voice" | "vc_text",
  startMs: number,
  text: string,
): TranscriptResponse["segments"][number] {
  return {
    id,
    speaker_id: "speaker-1",
    speaker: {
      id: "speaker-1",
      username: null,
      nickname: null,
      display_name: null,
      display_label: "Speaker",
    },
    start_ms: startMs,
    end_ms: startMs + 500,
    text,
    confidence: null,
    is_noisy: false,
    source,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function finishTranscript(status = "summarizing") {
  const source = TestEventSource.latest;
  if (!source) {
    throw new Error("EventSource was not created");
  }
  source.emit("segments", JSON.stringify(transcript(status, true)));
}

beforeEach(() => {
  vi.mocked(fetchMeeting).mockReset();
  vi.mocked(fetchTranscript).mockReset();
  vi.mocked(fetchSummary).mockReset();
  TestEventSource.latest = null;
  TestEventSource.instances = [];
  vi.stubGlobal("EventSource", TestEventSource);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("useMeetingData completion tracking", () => {
  it("replaces live segments with the ordered final snapshot and merges later deltas", async () => {
    vi.mocked(fetchMeeting).mockResolvedValue(
      meeting("meeting-1", "recording"),
    );
    vi.mocked(fetchTranscript).mockResolvedValue({
      ...transcript("recording", false),
      segments: [segment("live-1", "voice", 1_000, "temporary live text")],
    });
    vi.mocked(fetchSummary).mockResolvedValue({ markdown: null });

    const { result } = renderHook(() => useMeetingData("meeting-1"));
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(result.current.transcript?.[0]?.id).toBe("live-1"),
    );
    const source = TestEventSource.latest;
    if (!source) {
      throw new Error("EventSource was not created");
    }

    act(() =>
      source.emit(
        "segments",
        JSON.stringify({
          ...transcript("summarizing", true),
          reset: true,
          segments: [
            segment("final-2", "voice", 2_000, "second final"),
            segment("final-1", "voice", 500, "first final"),
          ],
        }),
      ),
    );

    expect(result.current.transcript?.map(({ id }) => id)).toEqual([
      "final-1",
      "final-2",
    ]);

    act(() =>
      source.emit(
        "segments",
        JSON.stringify({
          ...transcript("summarizing", true),
          segments: [
            segment("final-3", "vc_text", 3_000, "later delta"),
            segment("final-2", "voice", 2_000, "updated second final"),
          ],
        }),
      ),
    );

    expect(result.current.transcript?.map(({ id }) => id)).toEqual([
      "final-1",
      "final-2",
      "final-3",
    ]);
    expect(result.current.transcript?.map(({ text }) => text)).toEqual([
      "first final",
      "updated second final",
      "later delta",
    ]);
  });

  it("fetches and displays the summary when a final transcript progresses to posted", async () => {
    let polledStatus = "summarizing";
    vi.mocked(fetchMeeting)
      .mockResolvedValueOnce(meeting("meeting-1", "recording"))
      .mockImplementation(async (id) => meeting(id, polledStatus));
    vi.mocked(fetchTranscript).mockImplementation(async () =>
      polledStatus === "summarizing"
        ? transcript("recording", false)
        : transcript(polledStatus, true),
    );
    vi.mocked(fetchSummary).mockResolvedValue({
      markdown: "Completed summary",
    });

    const { result } = renderHook(() => useMeetingData("meeting-1"));
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(result.current.transcriptState?.status).toBe("recording"),
    );
    vi.useFakeTimers();
    act(() => finishTranscript());

    expect(result.current.meeting?.status).toBe("summarizing");
    expect(fetchSummary).not.toHaveBeenCalled();

    polledStatus = "posted";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });

    expect(result.current.meeting?.status).toBe("posted");
    expect(result.current.summary).toEqual<SummaryResponse>({
      markdown: "Completed summary",
    });
    expect(fetchSummary).toHaveBeenCalledTimes(1);
  });

  it.each([
    "failed",
    "aborted",
  ])("reflects a terminal %s meeting status after the final transcript", async (status) => {
    let polledStatus = "summarizing";
    vi.mocked(fetchMeeting)
      .mockResolvedValueOnce(meeting("meeting-1", "recording"))
      .mockImplementation(async (id) => meeting(id, polledStatus));
    vi.mocked(fetchTranscript).mockImplementation(async () =>
      polledStatus === "summarizing"
        ? transcript("recording", false)
        : transcript(polledStatus, true),
    );
    vi.mocked(fetchSummary).mockResolvedValue({ markdown: null });

    const { result } = renderHook(() => useMeetingData("meeting-1"));
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(result.current.transcriptState?.status).toBe("recording"),
    );
    vi.useFakeTimers();
    act(() => finishTranscript());
    polledStatus = status;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });

    expect(result.current.meeting?.status).toBe(status);
  });

  it("tracks meeting completion after idle timeout without reconnecting the transcript", async () => {
    let polledStatus = "recording";
    vi.mocked(fetchMeeting)
      .mockResolvedValueOnce(meeting("meeting-1", "recording"))
      .mockImplementation(async (id) => meeting(id, polledStatus));
    vi.mocked(fetchTranscript).mockImplementation(async () =>
      polledStatus === "recording"
        ? transcript("recording", false)
        : transcript(polledStatus, true),
    );
    vi.mocked(fetchSummary).mockResolvedValue({
      markdown: "After idle summary",
    });

    const { result } = renderHook(() => useMeetingData("meeting-1"));
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(result.current.transcriptState?.status).toBe("recording"),
    );
    const source = TestEventSource.latest;
    if (!source) {
      throw new Error("EventSource was not created");
    }
    vi.useFakeTimers();
    act(() =>
      source.emit("stream-closed", JSON.stringify({ code: "idle_timeout" })),
    );
    polledStatus = "posted";

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });

    expect(result.current.meeting?.status).toBe("posted");
    expect(result.current.summary?.markdown).toBe("After idle summary");
    expect(source.close).toHaveBeenCalledTimes(1);
    expect(TestEventSource.instances).toHaveLength(1);
  });

  it("tracks meeting completion after an error-limit stream closure", async () => {
    let polledStatus = "recording";
    vi.mocked(fetchMeeting)
      .mockResolvedValueOnce(meeting("meeting-1", "recording"))
      .mockImplementation(async (id) => meeting(id, polledStatus));
    vi.mocked(fetchTranscript).mockImplementation(async () =>
      polledStatus === "recording"
        ? transcript("recording", false)
        : transcript(polledStatus, true),
    );
    vi.mocked(fetchSummary).mockResolvedValue({
      markdown: "After error summary",
    });

    const { result } = renderHook(() => useMeetingData("meeting-1"));
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(result.current.transcriptState?.status).toBe("recording"),
    );
    const source = TestEventSource.latest;
    if (!source) {
      throw new Error("EventSource was not created");
    }
    vi.useFakeTimers();
    act(() =>
      source.emit("stream-closed", JSON.stringify({ code: "error_limit" })),
    );
    polledStatus = "posted";

    expect(result.current.transcriptStreamState).toBe("error");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });

    expect(result.current.meeting?.status).toBe("posted");
    expect(result.current.summary?.markdown).toBe("After error summary");
    expect(source.close).toHaveBeenCalledTimes(1);
    expect(TestEventSource.instances).toHaveLength(1);
  });

  it("does not poll completion after a not-found stream error", async () => {
    vi.mocked(fetchMeeting).mockResolvedValue(
      meeting("meeting-1", "recording"),
    );
    vi.mocked(fetchTranscript).mockResolvedValue(
      transcript("recording", false),
    );
    vi.mocked(fetchSummary).mockResolvedValue({ markdown: null });

    const { result } = renderHook(() => useMeetingData("meeting-1"));
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(result.current.transcriptState?.status).toBe("recording"),
    );
    const source = TestEventSource.latest;
    if (!source) {
      throw new Error("EventSource was not created");
    }
    vi.useFakeTimers();
    act(() =>
      source.emit("stream-error", JSON.stringify({ code: "not_found" })),
    );
    const meetingRequests = vi.mocked(fetchMeeting).mock.calls.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });

    expect(result.current.transcriptStreamState).toBe("error");
    expect(vi.mocked(fetchMeeting).mock.calls).toHaveLength(meetingRequests);
    expect(TestEventSource.instances).toHaveLength(1);
  });

  it.each([
    403, 404,
  ])("stops completion polling after a permanent %s meeting lookup failure", async (status) => {
    vi.mocked(fetchMeeting)
      .mockResolvedValueOnce(meeting("meeting-1", "recording"))
      .mockRejectedValue(
        new Error(`${status} ${status === 403 ? "Forbidden" : "Not Found"}`),
      );
    vi.mocked(fetchTranscript).mockResolvedValue(
      transcript("recording", false),
    );
    vi.mocked(fetchSummary).mockResolvedValue({ markdown: null });

    const { result } = renderHook(() => useMeetingData("meeting-1"));
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(result.current.transcriptState?.status).toBe("recording"),
    );
    vi.useFakeTimers();
    act(() => finishTranscript());

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(result.current.error).toBe(
      status === 403
        ? "この会議を表示する権限がありません"
        : "会議が見つかりません",
    );
    const meetingRequests = vi.mocked(fetchMeeting).mock.calls.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });

    expect(vi.mocked(fetchMeeting).mock.calls).toHaveLength(meetingRequests);
  });

  it("aborts old completion polls on meeting changes and unmount", async () => {
    const oldPoll = deferred<MeetingResponse>();
    const oldPollSignals: AbortSignal[] = [];
    let meetingOneCalls = 0;
    vi.mocked(fetchMeeting).mockImplementation((id, signal) => {
      if (id === "meeting-1") {
        meetingOneCalls += 1;
        if (meetingOneCalls === 1) {
          return Promise.resolve(meeting(id, "recording"));
        }
        if (signal) {
          oldPollSignals.push(signal);
        }
        return oldPoll.promise;
      }
      return Promise.resolve(meeting(id, "recording"));
    });
    vi.mocked(fetchTranscript).mockResolvedValue(
      transcript("recording", false),
    );
    vi.mocked(fetchSummary).mockResolvedValue({ markdown: null });

    const hook = renderHook(({ id }: { id: string }) => useMeetingData(id), {
      initialProps: { id: "meeting-1" },
    });
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(hook.result.current.transcriptState?.status).toBe("recording"),
    );
    vi.useFakeTimers();
    act(() => finishTranscript());
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(oldPollSignals).toHaveLength(1);

    hook.rerender({ id: "meeting-2" });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(oldPollSignals[0]?.aborted).toBe(true);
    oldPoll.resolve(meeting("meeting-1", "posted"));
    await act(async () => {
      await oldPoll.promise;
    });
    expect(hook.result.current.meeting).toMatchObject({
      id: "meeting-2",
      status: "recording",
    });

    const requestCount = vi.mocked(fetchMeeting).mock.calls.length;
    hook.unmount();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(vi.mocked(fetchMeeting).mock.calls).toHaveLength(requestCount);
  });

  it("stops completion polling when the hook unmounts", async () => {
    vi.mocked(fetchMeeting).mockResolvedValue(
      meeting("meeting-1", "recording"),
    );
    vi.mocked(fetchTranscript).mockResolvedValue(
      transcript("recording", false),
    );
    vi.mocked(fetchSummary).mockResolvedValue({ markdown: null });

    const hook = renderHook(() => useMeetingData("meeting-1"));
    await waitFor(() => expect(TestEventSource.latest).not.toBeNull());
    await waitFor(() =>
      expect(hook.result.current.transcriptState?.status).toBe("recording"),
    );
    vi.useFakeTimers();
    act(() => finishTranscript());
    const requestCount = vi.mocked(fetchMeeting).mock.calls.length;
    hook.unmount();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(vi.mocked(fetchMeeting).mock.calls).toHaveLength(requestCount);
  });
});
