import { useCallback, useEffect, useRef, useState } from "react";
import {
  fetchMeeting,
  fetchSummary,
  fetchTranscript,
  getTranscriptEventsUrl,
  isApiError,
  isTranscriptResponseValidationError,
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

export interface TranscriptSegmentIndex {
  segments: TranscriptSegment[];
  byKey: Map<string, TranscriptSegment>;
}

function compareTranscriptSegments(
  a: TranscriptSegment,
  b: TranscriptSegment,
): number {
  return (
    a.start_ms - b.start_ms ||
    a.end_ms - b.end_ms ||
    a.speaker_id.localeCompare(b.speaker_id) ||
    transcriptSegmentKey(a).localeCompare(transcriptSegmentKey(b))
  );
}

export function createTranscriptSegmentIndex(
  segments: TranscriptSegment[],
): TranscriptSegmentIndex {
  const byKey = new Map<string, TranscriptSegment>();
  for (const segment of segments) {
    byKey.set(transcriptSegmentKey(segment), segment);
  }
  const ordered = Array.from(byKey.values()).sort(compareTranscriptSegments);
  return { segments: ordered, byKey };
}

function transcriptSegmentPosition(
  segments: TranscriptSegment[],
  target: TranscriptSegment,
): number {
  let low = 0;
  let high = segments.length;
  while (low < high) {
    const middle = low + Math.floor((high - low) / 2);
    const middleSegment = segments[middle];
    if (middleSegment && compareTranscriptSegments(middleSegment, target) < 0) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  return low;
}

export function mergeTranscriptSegmentIndex(
  current: TranscriptSegmentIndex,
  incoming: TranscriptSegment[],
): TranscriptSegmentIndex {
  if (incoming.length === 0) {
    return current;
  }

  const segments = current.segments.slice();
  for (const segment of incoming) {
    const key = transcriptSegmentKey(segment);
    const existing = current.byKey.get(key);
    if (existing) {
      const existingPosition = transcriptSegmentPosition(segments, existing);
      if (compareTranscriptSegments(existing, segment) === 0) {
        segments[existingPosition] = segment;
        current.byKey.set(key, segment);
        continue;
      }
      segments.splice(existingPosition, 1);
    }

    const insertAt = transcriptSegmentPosition(segments, segment);
    segments.splice(insertAt, 0, segment);
    current.byKey.set(key, segment);
  }
  return { segments, byKey: current.byKey };
}

function isForbiddenError(error: unknown): boolean {
  return isApiError(error) && error.status === 403;
}

function isNotFoundError(error: unknown): boolean {
  return isApiError(error) && error.status === 404;
}

function transcriptFetchErrorMessage(retryAttempt: number): string {
  return retryAttempt > 0
    ? "\u6587\u5b57\u8d77\u3053\u3057\u306e\u518d\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f"
    : "\u6587\u5b57\u8d77\u3053\u3057\u306e\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f";
}

const MEETING_COMPLETION_POLL_INTERVAL_MS = 5_000;

function streamEventCode(data: string): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    return "unknown";
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return "unknown";
  }
  const code = "code" in parsed ? parsed.code : undefined;
  return typeof code === "string" ? code : "unknown";
}

export function useMeetingData(
  meetingId: string | undefined,
  guildId?: string,
): MeetingData {
  const [meeting, setMeeting] = useState<MeetingResponse | null>(null);
  const [transcript, setTranscript] = useState<TranscriptSegment[] | null>(
    null,
  );
  const transcriptIndexRef = useRef(createTranscriptSegmentIndex([]));
  const [transcriptState, setTranscriptState] =
    useState<TranscriptStateResponse | null>(null);
  const [summary, setSummary] = useState<SummaryResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [transcriptError, setTranscriptError] = useState<string | null>(null);
  const [summaryError, setSummaryError] = useState<string | null>(null);
  const [transcriptStreamState, setTranscriptStreamState] =
    useState<TranscriptStreamState>("idle");
  const [
    meetingCompletionTrackingEnabled,
    setMeetingCompletionTrackingEnabled,
  ] = useState(false);
  const [transcriptStreamError, setTranscriptStreamError] = useState<
    string | null
  >(null);
  const [transcriptRetryCount, setTranscriptRetryCount] = useState(0);
  const [summaryRetryCount, setSummaryRetryCount] = useState(0);
  const shouldStreamTranscript = isLiveMeetingStatus(meeting?.status);

  const replaceTranscript = useCallback(
    (segments: TranscriptSegment[] | null) => {
      const index = createTranscriptSegmentIndex(segments ?? []);
      transcriptIndexRef.current = index;
      setTranscript(segments === null ? null : index.segments);
    },
    [],
  );

  const mergeTranscript = useCallback((segments: TranscriptSegment[]) => {
    const index = mergeTranscriptSegmentIndex(
      transcriptIndexRef.current,
      segments,
    );
    transcriptIndexRef.current = index;
    setTranscript(index.segments);
  }, []);

  const applyTranscriptStatus = useCallback((response: TranscriptResponse) => {
    setTranscriptState(transcriptStateFromResponse(response));
    if (response.is_final) {
      setMeetingCompletionTrackingEnabled(true);
    }
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
      setMeetingCompletionTrackingEnabled(false);
      replaceTranscript(null);
      return;
    }

    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setMeeting(null);
    setMeetingCompletionTrackingEnabled(false);
    setTranscriptStreamState("idle");
    setTranscriptStreamError(null);
    setTranscriptRetryCount(0);
    setSummaryRetryCount(0);

    Promise.all([
      fetchMeeting(meetingId, controller.signal, guildId)
        .then((response) => {
          if (!controller.signal.aborted) {
            setMeeting(response);
          }
        })
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
  }, [meetingId, guildId, replaceTranscript]);

  useEffect(() => {
    const retryAttempt = transcriptRetryCount;
    if (!meetingId) {
      replaceTranscript(null);
      setTranscriptState(null);
      setTranscriptError(null);
      return;
    }

    const controller = new AbortController();
    replaceTranscript(null);
    setTranscriptState(null);
    setTranscriptError(null);
    fetchTranscript(meetingId, controller.signal, guildId)
      .then((response) => {
        if (controller.signal.aborted) return;
        if (shouldStreamTranscript) {
          mergeTranscript(response.segments);
        } else {
          replaceTranscript(response.segments);
        }
        applyTranscriptStatus(response);
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setTranscriptError(transcriptFetchErrorMessage(retryAttempt));
        }
      });
    return () => controller.abort();
  }, [
    meetingId,
    guildId,
    transcriptRetryCount,
    shouldStreamTranscript,
    applyTranscriptStatus,
    mergeTranscript,
    replaceTranscript,
  ]);

  const shouldTrackMeetingCompletion =
    isLiveMeetingStatus(meeting?.status) && meetingCompletionTrackingEnabled;

  useEffect(() => {
    if (!meetingId || !shouldTrackMeetingCompletion) {
      return;
    }

    const controller = new AbortController();
    let timer: number | undefined;
    const pollMeetingStatus = async () => {
      try {
        const latestMeeting = await fetchMeeting(
          meetingId,
          controller.signal,
          guildId,
        );
        if (controller.signal.aborted) {
          return;
        }
        setMeeting(latestMeeting);
      } catch (err: unknown) {
        if (controller.signal.aborted) {
          return;
        }
        if (isForbiddenError(err) || isNotFoundError(err)) {
          setMeetingCompletionTrackingEnabled(false);
          setError(
            isForbiddenError(err)
              ? "この会議を表示する権限がありません"
              : "会議が見つかりません",
          );
          return;
        }
      }

      if (!controller.signal.aborted) {
        timer = window.setTimeout(
          pollMeetingStatus,
          MEETING_COMPLETION_POLL_INTERVAL_MS,
        );
      }
    };

    timer = window.setTimeout(
      pollMeetingStatus,
      MEETING_COMPLETION_POLL_INTERVAL_MS,
    );
    return () => {
      controller.abort();
      if (timer !== undefined) {
        window.clearTimeout(timer);
      }
    };
  }, [meetingId, guildId, shouldTrackMeetingCompletion]);

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
    let accessCheckController: AbortController | null = null;

    const closeStream = () => {
      if (closed) {
        return;
      }
      closed = true;
      source?.close();
      accessCheckController?.abort();
      accessCheckController = null;
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
      const controller = new AbortController();
      accessCheckController = controller;
      fetchTranscript(meetingId, controller.signal, guildId)
        .then((response) => {
          accessCheckInFlight = false;
          if (closed) {
            return;
          }
          accessCheckController = null;
          applyTranscriptStatus(response);
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
          accessCheckController = null;
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
          if (isTranscriptResponseValidationError(err)) {
            closeStream();
            setTranscriptStreamState("error");
            setTranscriptStreamError(null);
            setTranscriptError(
              transcriptFetchErrorMessage(transcriptRetryCount),
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
      setTranscriptStreamState(
        attempt === 0 && transcriptRetryCount === 0
          ? "connecting"
          : "reconnecting",
      );
      setTranscriptStreamError(null);
      source = new EventSource(getTranscriptEventsUrl(meetingId, guildId), {
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
            JSON.parse(message.data),
          );
          applyTranscriptStatus(response);
          if (response.segments.length > 0) {
            if (response.reset) {
              replaceTranscript(response.segments);
            } else {
              mergeTranscript(response.segments);
            }
          } else if (response.reset) {
            replaceTranscript([]);
          }
          if (response.is_final) {
            closeStream();
            setTranscriptStreamState("closed");
            setTranscriptStreamError(null);
            return;
          }
          setTranscriptStreamState("open");
          setTranscriptStreamError(null);
        } catch (err: unknown) {
          if (isTranscriptResponseValidationError(err)) {
            closeStream();
            setTranscriptStreamState("error");
            setTranscriptStreamError(null);
            setTranscriptError(
              transcriptFetchErrorMessage(transcriptRetryCount),
            );
            return;
          }
          setTranscriptStreamState("error");
          setTranscriptStreamError(
            "\u6587\u5b57\u8d77\u3053\u3057\u66f4\u65b0\u306e\u89e3\u6790\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
          );
        }
      });

      source.addEventListener("stream-error", (event) => {
        const message = event as MessageEvent<string>;
        const code = streamEventCode(message.data);
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

      source.addEventListener("stream-closed", (event) => {
        if (closed) {
          return;
        }
        const message = event as MessageEvent<string>;
        const code = streamEventCode(message.data);
        closeStream();
        if (code === "idle_timeout") {
          setMeetingCompletionTrackingEnabled(true);
          setTranscriptStreamState("closed");
          setTranscriptStreamError(null);
          return;
        }
        if (code === "error_limit") {
          setMeetingCompletionTrackingEnabled(true);
          setTranscriptStreamState("error");
          setTranscriptStreamError(
            "\u6587\u5b57\u8d77\u3053\u3057\u306e\u30e9\u30a4\u30d6\u66f4\u65b0\u3067\u30a8\u30e9\u30fc\u304c\u7d9a\u3044\u305f\u305f\u3081\u505c\u6b62\u3057\u307e\u3057\u305f",
          );
          return;
        }
        setTranscriptStreamState("error");
        setTranscriptStreamError(
          "\u6587\u5b57\u8d77\u3053\u3057\u306e\u30e9\u30a4\u30d6\u66f4\u65b0\u304c\u505c\u6b62\u3057\u307e\u3057\u305f",
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
  }, [
    meetingId,
    guildId,
    shouldStreamTranscript,
    transcriptRetryCount,
    applyTranscriptStatus,
    mergeTranscript,
    replaceTranscript,
  ]);

  useEffect(() => {
    const retryAttempt = summaryRetryCount;
    if (!meetingId) {
      setSummary(null);
      setSummaryError(null);
      return;
    }
    if (!meeting) {
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
    fetchSummary(meetingId, controller.signal, guildId)
      .then((response) => {
        if (!controller.signal.aborted) {
          setSummary(response);
        }
      })
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
  }, [meetingId, guildId, meeting, shouldStreamTranscript, summaryRetryCount]);

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
