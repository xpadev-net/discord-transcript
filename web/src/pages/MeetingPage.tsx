import { useEffect, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import { AudioPlayer } from "../components/AudioPlayer";
import { Header } from "../components/Header";
import { SpeakerAudioDownloads } from "../components/SpeakerAudioDownloads";
import { SummaryPanel } from "../components/SummaryPanel";
import { TranscriptPanel } from "../components/TranscriptPanel";
import { useAudioSync } from "../hooks/useAudioSync";
import { useMeetingData } from "../hooks/useMeetingData";
import { fetchSpeakers, getAudioUrl } from "../lib/api";
import type { SpeakerAudioInfo } from "../lib/types";

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

  const [speakers, setSpeakers] = useState<SpeakerAudioInfo[] | null>(null);
  const [speakersLoading, setSpeakersLoading] = useState(true);
  const [speakersError, setSpeakersError] = useState(false);

  useEffect(() => {
    if (meetingId) {
      document.title = meeting?.title || "Meeting";
    }
  }, [meetingId, meeting?.title]);

  useEffect(() => {
    if (!meetingId) {
      setSpeakers(null);
      setSpeakersLoading(false);
      setSpeakersError(false);
      return;
    }
    const controller = new AbortController();
    setSpeakers(null);
    setSpeakersError(false);
    setSpeakersLoading(true);
    fetchSpeakers(meetingId, controller.signal)
      .then((data) => {
        if (!controller.signal.aborted) {
          setSpeakers(data);
        }
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setSpeakersError(true);
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setSpeakersLoading(false);
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
            <SpeakerAudioDownloads
              meetingId={meetingId}
              speakers={speakers}
              loading={speakersLoading}
              error={speakersError}
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
