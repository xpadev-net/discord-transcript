import { useCallback, useEffect, useRef, useState } from "react";
import type { TranscriptSegment } from "../lib/types";

const SCROLL_COOLDOWN_MS = 3000;

export function useAudioSync(
  audioRef: React.RefObject<HTMLAudioElement | null>,
  containerRef: React.RefObject<HTMLDivElement | null>,
  segments: TranscriptSegment[] | null,
) {
  const [activeIndex, setActiveIndex] = useState(-1);
  const [seekNotice, setSeekNotice] = useState<string | null>(null);
  const seekNoticeTimeoutRef = useRef<ReturnType<typeof setTimeout>>(undefined);
  const prevIndexRef = useRef(-1);
  const userScrolledRef = useRef(false);
  const scrollTimeoutRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  // Reset active index when segments change (e.g. meeting navigation)
  useEffect(() => {
    if (!segments || segments.length === 0) {
      setActiveIndex(-1);
      prevIndexRef.current = -1;
      userScrolledRef.current = false;
      if (scrollTimeoutRef.current) clearTimeout(scrollTimeoutRef.current);
      setSeekNotice(null);
      if (seekNoticeTimeoutRef.current) {
        clearTimeout(seekNoticeTimeoutRef.current);
      }
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
        if (
          currentMs >= segments[i].start_ms &&
          currentMs < segments[i].end_ms
        ) {
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
            const offset =
              segRect.top - containerRect.top - containerRect.height / 3;
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

  const showSeekNotice = useCallback((message: string) => {
    setSeekNotice(message);
    if (seekNoticeTimeoutRef.current) {
      clearTimeout(seekNoticeTimeoutRef.current);
    }
    seekNoticeTimeoutRef.current = setTimeout(() => {
      setSeekNotice(null);
    }, 4000);
  }, []);

  const seekTo = useCallback(
    (startMs: number) => {
      const audio = audioRef.current;
      if (!audio) {
        showSeekNotice(
          "\u97f3\u58f0\u30d7\u30ec\u30fc\u30e4\u30fc\u304c\u5229\u7528\u3067\u304d\u307e\u305b\u3093",
        );
        return;
      }
      if (audio.error) {
        showSeekNotice(
          "\u97f3\u58f0\u306e\u8aad\u307f\u8fbc\u307f\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        );
        return;
      }
      if (audio.readyState < HTMLMediaElement.HAVE_METADATA) {
        showSeekNotice(
          "\u97f3\u58f0\u306e\u8aad\u307f\u8fbc\u307f\u304c\u5b8c\u4e86\u3057\u3066\u3044\u307e\u305b\u3093",
        );
        return;
      }
      setSeekNotice(null);
      if (seekNoticeTimeoutRef.current) {
        clearTimeout(seekNoticeTimeoutRef.current);
      }
      try {
        audio.currentTime = startMs / 1000;
      } catch {
        showSeekNotice(
          "\u518d\u751f\u4f4d\u7f6e\u306e\u79fb\u52d5\u306b\u5931\u6557\u3057\u307e\u3057\u305f",
        );
        return;
      }
      void audio.play().catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") {
          return;
        }
        showSeekNotice(
          "\u97f3\u58f0\u306e\u518d\u751f\u3092\u958b\u59cb\u3067\u304d\u307e\u305b\u3093\u3067\u3057\u305f",
        );
      });
    },
    [audioRef, showSeekNotice],
  );

  useEffect(() => {
    return () => {
      if (seekNoticeTimeoutRef.current) {
        clearTimeout(seekNoticeTimeoutRef.current);
      }
    };
  }, []);

  return { activeIndex, seekTo, seekNotice };
}
