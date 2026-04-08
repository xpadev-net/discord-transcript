import { useEffect, useRef } from "react";
import { useParams } from "react-router-dom";
import { Header } from "../components/Header";
import { AudioPlayer } from "../components/AudioPlayer";
import { TranscriptPanel } from "../components/TranscriptPanel";
import { SummaryPanel } from "../components/SummaryPanel";
import { useMeetingData } from "../hooks/useMeetingData";
import { useAudioSync } from "../hooks/useAudioSync";
import { getAudioUrl } from "../lib/api";

export function MeetingPage() {
  const { meetingId } = useParams<{ meetingId: string }>();
  const audioRef = useRef<HTMLAudioElement>(null);
  const transcriptContainerRef = useRef<HTMLDivElement>(null);

  const { meeting, transcript, summary, loading, error } = useMeetingData(meetingId);
  const { activeIndex, seekTo } = useAudioSync(
    audioRef,
    transcriptContainerRef,
    transcript,
  );

  useEffect(() => {
    if (meetingId) {
      document.title = meeting?.title || "Meeting";
    }
  }, [meetingId, meeting?.title]);

  if (error) {
    return (
      <>
        <Header meeting={null} />
        <div className="empty-state">
          {error}
        </div>
      </>
    );
  }

  return (
    <>
      <Header meeting={meeting} />
      <div className="main-container">
        <div className="left-panel">
          <AudioPlayer key={meetingId} ref={audioRef} src={meetingId ? getAudioUrl(meetingId) : ""} />
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
