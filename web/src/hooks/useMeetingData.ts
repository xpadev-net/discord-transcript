import { useEffect, useState } from "react";
import type { MeetingResponse, TranscriptSegment, SummaryResponse } from "../lib/types";
import { fetchMeeting, fetchTranscript, fetchSummary } from "../lib/api";

interface MeetingData {
  meeting: MeetingResponse | null;
  transcript: TranscriptSegment[] | null;
  summary: SummaryResponse | null;
  loading: boolean;
  error: string | null;
}

export function useMeetingData(meetingId: string | undefined): MeetingData {
  const [meeting, setMeeting] = useState<MeetingResponse | null>(null);
  const [transcript, setTranscript] = useState<TranscriptSegment[] | null>(null);
  const [summary, setSummary] = useState<SummaryResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!meetingId) return;

    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setMeeting(null);
    setTranscript(null);
    setSummary(null);

    Promise.all([
      fetchMeeting(meetingId, controller.signal).then(setMeeting).catch(() => {
        if (!controller.signal.aborted) {
          setError("\u4f1a\u8b70\u60c5\u5831\u306e\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f");
        }
      }),
      fetchTranscript(meetingId, controller.signal).then(setTranscript).catch(() => {
        if (!controller.signal.aborted) setTranscript([]);
      }),
      fetchSummary(meetingId, controller.signal).then(setSummary).catch(() => {
        if (!controller.signal.aborted) setSummary({ markdown: null });
      }),
    ]).finally(() => {
      if (!controller.signal.aborted) setLoading(false);
    });

    return () => controller.abort();
  }, [meetingId]);

  return { meeting, transcript, summary, loading, error };
}
