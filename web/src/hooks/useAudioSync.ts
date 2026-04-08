import { useCallback, useEffect, useRef, useState } from "react";
import type { TranscriptSegment } from "../lib/types";

const SCROLL_COOLDOWN_MS = 3000;

export function useAudioSync(
  audioRef: React.RefObject<HTMLAudioElement | null>,
  containerRef: React.RefObject<HTMLDivElement | null>,
  segments: TranscriptSegment[] | null,
) {
  const [activeIndex, setActiveIndex] = useState(-1);
  const userScrolledRef = useRef(false);
  const scrollTimeoutRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  // Track user scroll to pause auto-scroll
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const handleScroll = () => {
      userScrolledRef.current = true;
      if (scrollTimeoutRef.current) clearTimeout(scrollTimeoutRef.current);
      scrollTimeoutRef.current = setTimeout(() => {
        userScrolledRef.current = false;
      }, SCROLL_COOLDOWN_MS);
    };

    container.addEventListener("scroll", handleScroll);
    return () => {
      container.removeEventListener("scroll", handleScroll);
      if (scrollTimeoutRef.current) clearTimeout(scrollTimeoutRef.current);
    };
  }, [containerRef]);

  // Sync active segment with audio time
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio || !segments || segments.length === 0) return;

    const handleTimeUpdate = () => {
      const currentMs = audio.currentTime * 1000;
      let newIndex = -1;

      for (let i = 0; i < segments.length; i++) {
        if (currentMs >= segments[i].start_ms && currentMs < segments[i].end_ms) {
          newIndex = i;
          break;
        }
      }

      setActiveIndex((prev) => {
        if (newIndex === prev) return prev;

        // Auto-scroll to active segment
        if (newIndex >= 0 && !userScrolledRef.current) {
          const container = containerRef.current;
          if (container) {
            const segmentEls = container.querySelectorAll(".segment");
            if (newIndex < segmentEls.length) {
              const segEl = segmentEls[newIndex];
              const containerRect = container.getBoundingClientRect();
              const segRect = segEl.getBoundingClientRect();
              const offset = segRect.top - containerRect.top - containerRect.height / 3;
              container.scrollBy({ top: offset, behavior: "smooth" });
            }
          }
        }

        return newIndex;
      });
    };

    audio.addEventListener("timeupdate", handleTimeUpdate);
    return () => audio.removeEventListener("timeupdate", handleTimeUpdate);
  }, [audioRef, containerRef, segments]);

  const seekTo = useCallback(
    (startMs: number) => {
      const audio = audioRef.current;
      if (audio) {
        audio.currentTime = startMs / 1000;
        audio.play();
      }
    },
    [audioRef],
  );

  return { activeIndex, seekTo };
}
