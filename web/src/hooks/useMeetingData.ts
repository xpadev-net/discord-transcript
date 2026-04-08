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

    setLoading(true);
    setError(null);

    Promise.all([
      fetchMeeting(meetingId).then(setMeeting).catch(() => {
        setError("\u4f1a\u8b70\u60c5\u5831\u306e\u53d6\u5f97\u306b\u5931\u6557\u3057\u307e\u3057\u305f");
      }),
      fetchTranscript(meetingId).then(setTranscript).catch(() => {
        setTranscript([]);
      }),
      fetchSummary(meetingId).then(setSummary).catch(() => {
        setSummary({ markdown: null });
      }),
    ]).finally(() => {
      setLoading(false);
    });
  }, [meetingId]);

  return { meeting, transcript, summary, loading, error };
}
