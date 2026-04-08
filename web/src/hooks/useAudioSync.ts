import { useCallback, useEffect, useRef, useState } from "react";
import type { TranscriptSegment } from "../lib/types";

const SCROLL_COOLDOWN_MS = 3000;

export function useAudioSync(
  audioRef: React.RefObject<HTMLAudioElement | null>,
  containerRef: React.RefObject<HTMLDivElement | null>,
  segments: TranscriptSegment[] | null,
) {
  const [activeIndex, setActiveIndex] = useState(-1);
  const prevIndexRef = useRef(-1);
  const userScrolledRef = useRef(false);
  const scrollTimeoutRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  // Reset active index when segments change (e.g. meeting navigation)
  useEffect(() => {
    if (!segments || segments.length === 0) {
      setActiveIndex(-1);
      prevIndexRef.current = -1;
    }
  }, [segments]);

  // Track user scroll via input events to avoid false positives from programmatic scrollBy
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const markUserScrolled = () => {
      userScrolledRef.current = true;
      if (scrollTimeoutRef.current) clearTimeout(scrollTimeoutRef.current);
      scrollTimeoutRef.current = setTimeout(() => {
        userScrolledRef.current = false;
      }, SCROLL_COOLDOWN_MS);
    };

    container.addEventListener("wheel", markUserScrolled);
    container.addEventListener("touchstart", markUserScrolled);
    container.addEventListener("pointerdown", markUserScrolled);
    container.addEventListener("keydown", markUserScrolled);
    return () => {
      container.removeEventListener("wheel", markUserScrolled);
      container.removeEventListener("touchstart", markUserScrolled);
      container.removeEventListener("pointerdown", markUserScrolled);
      container.removeEventListener("keydown", markUserScrolled);
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

      if (newIndex === prevIndexRef.current) return;

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

      prevIndexRef.current = newIndex;
      setActiveIndex(newIndex);
    };

    audio.addEventListener("timeupdate", handleTimeUpdate);
    return () => audio.removeEventListener("timeupdate", handleTimeUpdate);
  }, [audioRef, containerRef, segments]);

  const seekTo = useCallback(
    (startMs: number) => {
      const audio = audioRef.current;
      if (audio) {
        audio.currentTime = startMs / 1000;
        audio.play().catch(() => {});
      }
    },
    [audioRef],
  );

  return { activeIndex, seekTo };
}
