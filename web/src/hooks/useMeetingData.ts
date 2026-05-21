import { useEffect, useState } from "react";
import { fetchMeeting, fetchSummary, fetchTranscript } from "../lib/api";
import type {
  MeetingResponse,
  SummaryResponse,
  TranscriptSegment,
} from "../lib/types";

interface MeetingData {
  meeting: MeetingResponse | null;
  transcript: TranscriptSegment[] | null;
  summary: SummaryResponse | null;
  loading: boolean;
  error: string | null;
  transcriptError: string | null;
  summaryError: string | null;
  retryTranscript: () => void;
  retrySummary: () => void;
}

export function useMeetingData(meetingId: string | undefined): MeetingData {
  const [meeting, setMeeting] = useState<MeetingResponse | null>(null);
  const [transcript, setTranscript] = useState<TranscriptSegment[] | null>(
    null,
  );
  const [summary, setSummary] = useState<SummaryResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [transcriptError, setTranscriptError] = useState<string | null>(null);
  const [summaryError, setSummaryError] = useState<string | null>(null);
  const [transcriptRetryCount, setTranscriptRetryCount] = useState(0);
  const [summaryRetryCount, setSummaryRetryCount] = useState(0);

  useEffect(() => {
    if (!meetingId) {
      setLoading(false);
      return;
    }

    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setMeeting(null);

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
      setTranscriptError(null);
      return;
    }

    const controller = new AbortController();
    setTranscript(null);
    setTranscriptError(null);
    fetchTranscript(meetingId, controller.signal)
      .then(setTranscript)
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
  }, [meetingId, transcriptRetryCount]);

  useEffect(() => {
    const retryAttempt = summaryRetryCount;
    if (!meetingId) {
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
  }, [meetingId, summaryRetryCount]);

  return {
    meeting,
    transcript,
    summary,
    loading,
    error,
    transcriptError,
    summaryError,
    retryTranscript: () => setTranscriptRetryCount((count) => count + 1),
    retrySummary: () => setSummaryRetryCount((count) => count + 1),
  };
}
