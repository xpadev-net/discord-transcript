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
import { isLiveMeetingStatus } from "../lib/meetingStatus";
import type { DebugArtifact } from "../lib/types";

function inProgressMessage(status: string | undefined): string | null {
  if (status === "recording") {
    return "\u9332\u97f3\u4e2d\u3067\u3059\u3002\u6587\u5b57\u8d77\u3053\u3057\u304c\u5230\u7740\u3059\u308b\u3068\u3053\u306e\u30da\u30fc\u30b8\u306b\u8ffd\u52a0\u3055\u308c\u307e\u3059\u3002";
  }
  if (status === "stopping") {
    return "\u9332\u97f3\u306e\u505c\u6b62\u51e6\u7406\u4e2d\u3067\u3059\u3002\u97f3\u58f0\u3068\u6587\u5b57\u8d77\u3053\u3057\u3092\u6e96\u5099\u3057\u3066\u3044\u307e\u3059\u3002";
  }
  if (status === "transcribing" || status === "processing") {
    return "\u6587\u5b57\u8d77\u3053\u3057\u4e2d\u3067\u3059\u3002\u7d50\u679c\u306f\u9806\u6b21\u8ffd\u52a0\u3055\u308c\u307e\u3059\u3002";
  }
  if (status === "summarizing") {
    return "\u8981\u7d04\u4e2d\u3067\u3059\u3002\u6587\u5b57\u8d77\u3053\u3057\u306f\u5f15\u304d\u7d9a\u304d\u78ba\u8a8d\u3067\u304d\u307e\u3059\u3002";
  }
  return null;
}

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
    transcriptStreamState,
    transcriptStreamError,
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
  const isLive = isLiveMeetingStatus(meeting?.status);
  const progressMessage = inProgressMessage(meeting?.status);
  const showAudioAndDebug = meeting?.status !== "recording";

  useEffect(() => {
    if (meetingId) {
      document.title = meeting?.title || "Meeting";
    }
  }, [meetingId, meeting?.title]);

  useEffect(() => {
    if (!meetingId || !showAudioAndDebug) {
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
  }, [meetingId, showAudioAndDebug]);

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
          {progressMessage ? (
            <div className="meeting-progress-notice" role="status">
              {progressMessage}
            </div>
          ) : null}
          {showAudioAndDebug ? (
            <AudioPlayer
              key={meetingId}
              ref={audioRef}
              src={meetingId ? getAudioUrl(meetingId) : ""}
            />
          ) : (
            <div className="audio-container audio-placeholder" role="status">
              {
                "\u97f3\u58f0\u30d7\u30ec\u30fc\u30e4\u30fc\u306f\u9332\u97f3\u7d42\u4e86\u5f8c\u306b\u5229\u7528\u3067\u304d\u307e\u3059"
              }
            </div>
          )}
          {meetingId && showAudioAndDebug ? (
            <DebugDownloads
              artifacts={debugArtifacts}
              loading={debugLoading}
              error={debugError}
            />
          ) : null}
          {seekNotice ? (
            <div className="seek-notice" role="status">
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
              streamState={transcriptStreamState}
              streamError={transcriptStreamError}
              isLive={isLive}
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
            loading={
              !isLive &&
              !!meetingId &&
              summary === null &&
              summaryError === null
            }
            error={summaryError}
            onRetry={retrySummary}
          />
        </ErrorBoundary>
      </div>
    </>
  );
}
