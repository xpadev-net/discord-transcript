import { useEffect, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import { AudioPlayer } from "../components/AudioPlayer";
import { DebugDownloads } from "../components/DebugDownloads";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { Header } from "../components/Header";
import { SummaryPanel } from "../components/SummaryPanel";
import { TranscriptPanel } from "../components/TranscriptPanel";
import { useAudioSync } from "../hooks/useAudioSync";
import { useMeetingData } from "../hooks/useMeetingData";
import { fetchDebugManifest, getAudioUrl } from "../lib/api";
import type { DebugArtifact } from "../lib/types";

export function MeetingPage() {
  const { meetingId } = useParams<{ meetingId: string }>();
  const audioRef = useRef<HTMLAudioElement>(null);
  const transcriptContainerRef = useRef<HTMLDivElement>(null);

  const {
    meeting,
    transcript,
    summary,
    error,
    transcriptError,
    summaryError,
    retryTranscript,
    retrySummary,
  } = useMeetingData(meetingId);
  const { activeIndex, seekTo, seekNotice } = useAudioSync(
    audioRef,
    transcriptContainerRef,
    transcript,
  );

  const [debugArtifacts, setDebugArtifacts] = useState<DebugArtifact[] | null>(
    null,
  );
  const [debugLoading, setDebugLoading] = useState(true);
  const [debugError, setDebugError] = useState(false);

  useEffect(() => {
    if (meetingId) {
      document.title = meeting?.title || "Meeting";
    }
  }, [meetingId, meeting?.title]);

  useEffect(() => {
    if (!meetingId) {
      setDebugArtifacts(null);
      setDebugLoading(false);
      setDebugError(false);
      return;
    }
    const controller = new AbortController();
    setDebugArtifacts(null);
    setDebugError(false);
    setDebugLoading(true);
    fetchDebugManifest(meetingId, controller.signal)
      .then((data) => {
        if (!controller.signal.aborted) {
          setDebugArtifacts(data);
        }
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setDebugError(true);
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setDebugLoading(false);
        }
      });
    return () => controller.abort();
  }, [meetingId]);

  if (error) {
    return (
      <>
        <Header meeting={null} />
        <div className="empty-state">{error}</div>
      </>
    );
  }

  return (
    <>
      <Header meeting={meeting} />
      <div className="main-container">
        <div className="left-panel">
          <AudioPlayer
            key={meetingId}
            ref={audioRef}
            src={meetingId ? getAudioUrl(meetingId) : ""}
          />
          {meetingId && (
            <DebugDownloads
              artifacts={debugArtifacts}
              loading={debugLoading}
              error={debugError}
            />
          )}
          {seekNotice ? (
            <div className="panel-error seek-notice" role="status">
              {seekNotice}
            </div>
          ) : null}
          <ErrorBoundary
            title={
              "\u30c8\u30e9\u30f3\u30b9\u30af\u30ea\u30d7\u30c8\u306e\u8868\u793a\u306b\u5931\u6557\u3057\u307e\u3057\u305f"
            }
          >
            <TranscriptPanel
              ref={transcriptContainerRef}
              segments={transcript}
              activeIndex={activeIndex}
              onSeek={seekTo}
              error={transcriptError}
              onRetry={retryTranscript}
            />
          </ErrorBoundary>
        </div>
        <ErrorBoundary
          title={
            "\u30b5\u30de\u30ea\u30fc\u306e\u8868\u793a\u306b\u5931\u6557\u3057\u307e\u3057\u305f"
          }
        >
          <SummaryPanel
            markdown={summary?.markdown}
            loading={!!meetingId && summary === null && summaryError === null}
            error={summaryError}
            onRetry={retrySummary}
          />
        </ErrorBoundary>
      </div>
    </>
  );
}
