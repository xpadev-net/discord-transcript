import { useCallback, useEffect, useState } from "react";
import {
  fetchMeeting,
  fetchSummary,
  fetchTranscript,
  getTranscriptEventsUrl,
  normalizeTranscriptResponse,
} from "../lib/api";
import { isLiveMeetingStatus } from "../lib/meetingStatus";
import type {
  MeetingResponse,
  SummaryResponse,
  TranscriptResponse,
  TranscriptSegment,
  TranscriptStateResponse,
  TranscriptStreamState,
} from "../lib/types";

interface MeetingData {
  meeting: MeetingResponse | null;
  transcript: TranscriptSegment[] | null;
  transcriptState: TranscriptStateResponse | null;
  summary: SummaryResponse | null;
  loading: boolean;
  error: string | null;
  transcriptError: string | null;
  summaryError: string | null;
  transcriptStreamState: TranscriptStreamState;
  transcriptStreamError: string | null;
  retryTranscript: () => void;
  retrySummary: () => void;
}

function transcriptStateFromResponse(
  response: TranscriptResponse,
): TranscriptStateResponse {
  return {
    status: response.status,
    is_final: response.is_final,
    updated_at: response.updated_at,
  };
}

function transcriptSegmentKey(segment: TranscriptSegment): string {
  return (
    segment.id ??
    [
      segment.source,
      segment.speaker_id,
      segment.start_ms,
      segment.end_ms,
      segment.text,
    ].join(":")
  );
}

function mergeTranscriptSegments(
  current: TranscriptSegment[] | null,
  incoming: TranscriptSegment[],
): TranscriptSegment[] {
  const byKey = new Map<string, TranscriptSegment>();
  for (const segment of current ?? []) {
    byKey.set(transcriptSegmentKey(segment), segment);
  }
  for (const segment of incoming) {
    byKey.set(transcriptSegmentKey(segment), segment);
  }
  return Array.from(byKey.values()).sort(
    (a, b) =>
      a.start_ms - b.start_ms ||
      a.end_ms - b.end_ms ||
      a.speaker_id.localeCompare(b.speaker_id) ||
      transcriptSegmentKey(a).localeCompare(transcriptSegmentKey(b)),
  );
}

function isForbiddenError(error: unknown): boolean {
  return error instanceof Error && error.message.startsWith("403");
}

function isNotFoundError(error: unknown): boolean {
  return error instanceof Error && error.message.startsWith("404");
}

export function useMeetingData(meetingId: string | undefined): MeetingData {
  const [meeting, setMeeting] = useState<MeetingResponse | null>(null);
  const [transcript, setTranscript] = useState<TranscriptSegment[] | null>(
    null,
  );
  const [transcriptState, setTranscriptState] =
    useState<TranscriptStateResponse | null>(null);
  const [summary, setSummary] = useState<SummaryResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [transcriptError, setTranscriptError] = useState<string | null>(null);
  const [summaryError, setSummaryError] = useState<string | null>(null);
  const [transcriptStreamState, setTranscriptStreamState] =
    useState<TranscriptStreamState>("idle");
  const [transcriptStreamError, setTranscriptStreamError] = useState<
    string | null
  >(null);
  const [transcriptRetryCount, setTranscriptRetryCount] = useState(0);
  const [summaryRetryCount, setSummaryRetryCount] = useState(0);
  const shouldStreamTranscript = isLiveMeetingStatus(meeting?.status);

  const applyTranscriptStatus = useCallback((response: TranscriptResponse) => {
    setTranscriptState(transcriptStateFromResponse(response));
    if (response.status === "unknown") {
      return;
    }
    setMeeting((current) =>
      current && current.status !== response.status
        ? { ...current, status: response.status }
        : current,
    );
  }, []);

  useEffect(() => {
    if (!meetingId) {
      setLoading(false);
      return;
    }

    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setMeeting(null);
    setTranscriptStreamState("idle");
    setTranscriptStreamError(null);
    setTranscriptRetryCount(0);
    setSummaryRetryCount(0);

    Promise.all([
      fetchMeeting(meetingId, controller.signal)
        .then(setMeeting)
        .catch(() => {
          if (!controller.signal.aborted) {
            setError(
              "\u4f1a\u8b70\u60c5\u5831\u306e\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
            );
          }
        }),
    ]).finally(() => {
      if (!controller.signal.aborted) setLoading(false);
    });

    return () => controller.abort();
  }, [meetingId]);

  useEffect(() => {
    const retryAttempt = transcriptRetryCount;
    if (!meetingId) {
      setTranscript(null);
      setTranscriptState(null);
      setTranscriptError(null);
      return;
    }

    const controller = new AbortController();
    setTranscript(null);
    setTranscriptState(null);
    setTranscriptError(null);
    fetchTranscript(meetingId, controller.signal)
      .then((response) => {
        if (controller.signal.aborted) return;
        setTranscript((current) =>
          shouldStreamTranscript
            ? mergeTranscriptSegments(current, response.segments)
            : response.segments,
        );
        applyTranscriptStatus(response);
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setTranscriptError(
            retryAttempt > 0
              ? "\u6587\u5b57\u8d77\u3053\u3057\u306e\u518d\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f"
              : "\u6587\u5b57\u8d77\u3053\u3057\u306e\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
          );
        }
      });
    return () => controller.abort();
  }, [
    meetingId,
    transcriptRetryCount,
    shouldStreamTranscript,
    applyTranscriptStatus,
  ]);

  useEffect(() => {
    if (!meetingId || !shouldStreamTranscript) {
      setTranscriptStreamState("idle");
      setTranscriptStreamError(null);
      return;
    }

    let closed = false;
    let reconnectTimer: number | undefined;
    let attempt = 0;
    let source: EventSource | null = null;
    let accessCheckInFlight = false;

    const closeStream = () => {
      closed = true;
      source?.close();
      if (reconnectTimer !== undefined) {
        window.clearTimeout(reconnectTimer);
      }
    };

    const scheduleReconnect = () => {
      if (closed) {
        return;
      }
      attempt += 1;
      const delayMs = Math.min(30000, 1000 * 2 ** Math.min(attempt, 5));
      setTranscriptStreamState("reconnecting");
      setTranscriptStreamError(
        "\u63a5\u7d9a\u304c\u5207\u308c\u307e\u3057\u305f\u3002\u518d\u63a5\u7d9a\u3057\u3066\u3044\u307e\u3059",
      );
      reconnectTimer = window.setTimeout(connect, delayMs);
    };

    const verifyAccessBeforeReconnect = () => {
      if (accessCheckInFlight) {
        return;
      }
      accessCheckInFlight = true;
      fetchTranscript(meetingId)
        .then((response) => {
          accessCheckInFlight = false;
          applyTranscriptStatus(response);
          if (closed) {
            return;
          }
          if (response.is_final) {
            closeStream();
            setTranscriptStreamState("closed");
            setTranscriptStreamError(null);
            return;
          }
          scheduleReconnect();
        })
        .catch((err: unknown) => {
          accessCheckInFlight = false;
          if (closed) {
            return;
          }
          if (isForbiddenError(err)) {
            closeStream();
            setTranscriptStreamState("forbidden");
            setTranscriptStreamError(
              "\u3053\u306e\u4f1a\u8b70\u306e\u6587\u5b57\u8d77\u3053\u3057\u3092\u8868\u793a\u3059\u308b\u6a29\u9650\u304c\u3042\u308a\u307e\u305b\u3093",
            );
            return;
          }
          if (isNotFoundError(err)) {
            closeStream();
            setTranscriptStreamState("error");
            setTranscriptStreamError(
              "\u4f1a\u8b70\u304c\u898b\u3064\u304b\u308a\u307e\u305b\u3093",
            );
            return;
          }
          scheduleReconnect();
        });
    };

    const connect = () => {
      if (closed) {
        return;
      }
      setTranscriptStreamState(attempt === 0 ? "connecting" : "reconnecting");
      setTranscriptStreamError(null);
      source = new EventSource(getTranscriptEventsUrl(meetingId), {
        withCredentials: true,
      });

      source.onopen = () => {
        attempt = 0;
        setTranscriptStreamState("open");
        setTranscriptStreamError(null);
      };

      source.addEventListener("segments", (event) => {
        const message = event as MessageEvent<string>;
        try {
          const response = normalizeTranscriptResponse(
            JSON.parse(message.data) as
              | TranscriptSegment[]
              | TranscriptResponse,
          );
          applyTranscriptStatus(response);
          if (response.segments.length > 0) {
            setTranscript((current) =>
              mergeTranscriptSegments(current, response.segments),
            );
          }
          if (response.is_final) {
            closeStream();
            setTranscriptStreamState("closed");
            setTranscriptStreamError(null);
            return;
          }
          setTranscriptStreamState("open");
          setTranscriptStreamError(null);
        } catch {
          setTranscriptStreamState("error");
          setTranscriptStreamError(
            "\u6587\u5b57\u8d77\u3053\u3057\u66f4\u65b0\u306e\u89e3\u6790\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
          );
        }
      });

      source.addEventListener("stream-error", (event) => {
        const message = event as MessageEvent<string>;
        let code = "unknown";
        try {
          code = (JSON.parse(message.data) as { code?: string }).code ?? code;
        } catch {
          // Keep the generic error code.
        }
        if (code === "forbidden") {
          closeStream();
          setTranscriptStreamState("forbidden");
          setTranscriptStreamError(
            "\u3053\u306e\u4f1a\u8b70\u306e\u6587\u5b57\u8d77\u3053\u3057\u3092\u8868\u793a\u3059\u308b\u6a29\u9650\u304c\u3042\u308a\u307e\u305b\u3093",
          );
          return;
        }
        if (code === "not_found") {
          closeStream();
          setTranscriptStreamState("error");
          setTranscriptStreamError(
            "\u4f1a\u8b70\u304c\u898b\u3064\u304b\u308a\u307e\u305b\u3093",
          );
          return;
        }
        setTranscriptStreamState("error");
        setTranscriptStreamError(
          "\u6587\u5b57\u8d77\u3053\u3057\u306e\u66f4\u65b0\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        );
      });

      source.onerror = () => {
        if (closed) {
          return;
        }
        source?.close();
        verifyAccessBeforeReconnect();
      };
    };

    connect();

    return () => {
      closeStream();
      setTranscriptStreamState("closed");
    };
  }, [meetingId, shouldStreamTranscript, applyTranscriptStatus]);

  useEffect(() => {
    const retryAttempt = summaryRetryCount;
    if (!meetingId) {
      setSummary(null);
      setSummaryError(null);
      return;
    }
    if (shouldStreamTranscript) {
      setSummary(null);
      setSummaryError(null);
      return;
    }

    const controller = new AbortController();
    setSummary(null);
    setSummaryError(null);
    fetchSummary(meetingId, controller.signal)
      .then(setSummary)
      .catch(() => {
        if (!controller.signal.aborted) {
          setSummaryError(
            retryAttempt > 0
              ? "\u30b5\u30de\u30ea\u30fc\u306e\u518d\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f"
              : "\u30b5\u30de\u30ea\u30fc\u306e\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
          );
        }
      });
    return () => controller.abort();
  }, [meetingId, shouldStreamTranscript, summaryRetryCount]);

  return {
    meeting,
    transcript,
    transcriptState,
    summary,
    loading,
    error,
    transcriptError,
    summaryError,
    transcriptStreamState,
    transcriptStreamError,
    retryTranscript: () => setTranscriptRetryCount((count) => count + 1),
    retrySummary: () => setSummaryRetryCount((count) => count + 1),
  };
}
