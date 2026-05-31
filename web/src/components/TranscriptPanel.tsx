import { forwardRef } from "react";
import type { TranscriptSegment, TranscriptStreamState } from "../lib/types";
import { LoadingSpinner } from "./LoadingSpinner";
import { TranscriptSegmentRow } from "./TranscriptSegment";

interface Props {
  segments: TranscriptSegment[] | null;
  activeIndex: number;
  onSeek: (startMs: number) => void;
  error: string | null;
  onRetry: () => void;
  streamState?: TranscriptStreamState;
  streamError?: string | null;
  isLive?: boolean;
}

function streamStatusText(
  streamState: TranscriptStreamState | undefined,
  streamError: string | null | undefined,
): string | null {
  if (!streamState || streamState === "idle" || streamState === "closed") {
    return null;
  }
  if (streamState === "open") {
    return "\u30e9\u30a4\u30d6\u66f4\u65b0\u4e2d";
  }
  if (streamState === "connecting") {
    return "\u30e9\u30a4\u30d6\u63a5\u7d9a\u4e2d";
  }
  if (streamState === "reconnecting") {
    return streamError ?? "\u518d\u63a5\u7d9a\u4e2d";
  }
  if (streamState === "forbidden") {
    return (
      streamError ??
      "\u6587\u5b57\u8d77\u3053\u3057\u3092\u8868\u793a\u3059\u308b\u6a29\u9650\u304c\u3042\u308a\u307e\u305b\u3093"
    );
  }
  return (
    streamError ??
    "\u6587\u5b57\u8d77\u3053\u3057\u306e\u30e9\u30a4\u30d6\u66f4\u65b0\u306b\u5931\u6557\u3057\u307e\u3057\u305f"
  );
}

export const TranscriptPanel = forwardRef<HTMLDivElement, Props>(
  function TranscriptPanel(
    {
      segments,
      activeIndex,
      onSeek,
      error,
      onRetry,
      streamState,
      streamError,
      isLive = false,
    },
    ref,
  ) {
    const liveStatus = streamStatusText(streamState, streamError);
    const liveStatusClass =
      streamState === "forbidden" || streamState === "error"
        ? "transcript-live-status is-error"
        : "transcript-live-status";

    const statusNode = liveStatus ? (
      <div className={liveStatusClass} role="status" aria-live="polite">
        {liveStatus}
      </div>
    ) : null;

    if (error) {
      return (
        <div className="transcript-container" ref={ref}>
          {statusNode}
          <div className="panel-error" role="alert">
            <div>{error}</div>
            <button type="button" onClick={onRetry}>
              {"\u518d\u8a66\u884c"}
            </button>
          </div>
        </div>
      );
    }

    if (segments === null) {
      return (
        <div className="transcript-container" ref={ref}>
          {statusNode}
          <LoadingSpinner text={"\u8aad\u307f\u8fbc\u307f\u4e2d..."} />
        </div>
      );
    }

    if (segments.length === 0) {
      return (
        <div className="transcript-container" ref={ref}>
          {statusNode}
          <div className="empty-state">
            {isLive
              ? "\u6587\u5b57\u8d77\u3053\u3057\u306e\u5230\u7740\u3092\u5f85\u3063\u3066\u3044\u307e\u3059"
              : "\u3053\u306e\u4f1a\u8b70\u306e\u6587\u5b57\u8d77\u3053\u3057\u306f\u307e\u3060\u5229\u7528\u3067\u304d\u307e\u305b\u3093"}
          </div>
        </div>
      );
    }

    return (
      <div className="transcript-container" ref={ref}>
        {statusNode}
        {segments.map((seg, i) => (
          <TranscriptSegmentRow
            key={seg.id ?? `${seg.speaker_id}-${seg.start_ms}-${seg.end_ms}`}
            segment={seg}
            isActive={i === activeIndex}
            onSeek={onSeek}
          />
        ))}
      </div>
    );
  },
);
