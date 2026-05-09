import { useEffect, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import { AudioPlayer } from "../components/AudioPlayer";
import { DebugDownloads } from "../components/DebugDownloads";
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

  const { meeting, transcript, summary, loading, error } =
    useMeetingData(meetingId);
  const { activeIndex, seekTo } = useAudioSync(
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
          <TranscriptPanel
            ref={transcriptContainerRef}
            segments={transcript}
            activeIndex={activeIndex}
            onSeek={seekTo}
          />
        </div>
        <SummaryPanel
          markdown={summary?.markdown}
          loading={loading && summary === null}
        />
      </div>
    </>
  );
}
