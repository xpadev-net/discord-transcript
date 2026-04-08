import { forwardRef } from "react";
import type { TranscriptSegment } from "../lib/types";
import { TranscriptSegmentRow } from "./TranscriptSegment";
import { LoadingSpinner } from "./LoadingSpinner";

interface Props {
  segments: TranscriptSegment[] | null;
  activeIndex: number;
  onSeek: (startMs: number) => void;
}

export const TranscriptPanel = forwardRef<HTMLDivElement, Props>(
  function TranscriptPanel({ segments, activeIndex, onSeek }, ref) {
    if (segments === null) {
      return (
        <div className="transcript-container" ref={ref}>
          <LoadingSpinner text={"\u8aad\u307f\u8fbc\u307f\u4e2d..."} />
        </div>
      );
    }

    if (segments.length === 0) {
      return (
        <div className="transcript-container" ref={ref}>
          <div className="empty-state">
            {"\u3053\u306e\u4f1a\u8b70\u306e\u6587\u5b57\u8d77\u3053\u3057\u306f\u307e\u3060\u5229\u7528\u3067\u304d\u307e\u305b\u3093"}
          </div>
        </div>
      );
    }

    return (
      <div className="transcript-container" ref={ref}>
        {segments.map((seg, i) => (
          <TranscriptSegmentRow
            key={`${seg.speaker_id}-${seg.start_ms}`}
            segment={seg}
            isActive={i === activeIndex}
            onSeek={onSeek}
          />
        ))}
      </div>
    );
  },
);
