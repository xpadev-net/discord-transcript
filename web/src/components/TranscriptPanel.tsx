import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { TranscriptSegment, TranscriptStreamState } from "../lib/types";
import { LoadingSpinner } from "./LoadingSpinner";
import { TranscriptSegmentRow } from "./TranscriptSegment";

interface Props {
  segments: TranscriptSegment[] | null;
  activeIndex: number;
  onSeek: (startMs: number) => void;
  onFeedback?: (segment: TranscriptSegment, returnFocusTo: HTMLElement) => void;
  error: string | null;
  onRetry: () => void;
  streamState?: TranscriptStreamState;
  streamError?: string | null;
  isLive?: boolean;
}

const EMPTY_SEGMENTS: TranscriptSegment[] = [];

function segmentKey(segment: TranscriptSegment): string {
  return (
    segment.id ?? `${segment.speaker_id}-${segment.start_ms}-${segment.end_ms}`
  );
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
      onFeedback,
      error,
      onRetry,
      streamState,
      streamError,
      isLive = false,
    },
    ref,
  ) {
    const containerRef = useRef<HTMLDivElement>(null);
    useImperativeHandle(ref, () => {
      const container = containerRef.current;
      if (!container) throw new Error("Transcript container is not mounted");
      return container;
    }, []);
    const [viewport, setViewport] = useState({ top: 0, height: 600, width: 0 });
    const [focusedIndex, setFocusedIndex] = useState<number | null>(null);
    const [pinnedFeedbackId, setPinnedFeedbackId] = useState<string | null>(
      null,
    );
    const [pendingFocus, setPendingFocus] = useState<{
      index: number;
      control: "segment" | "feedback";
    } | null>(null);
    const [measurements, setMeasurements] = useState(() => ({
      heights: new WeakMap<
        TranscriptSegment,
        { height: number; width: number }
      >(),
      revision: 0,
    }));
    const measurementsRef = useRef(measurements);
    useLayoutEffect(() => {
      measurementsRef.current = measurements;
    }, [measurements]);
    const rowNodes = useRef(new Map<number, HTMLLIElement>());
    const rowSegments = useRef(new WeakMap<HTMLLIElement, TranscriptSegment>());
    const observerRef = useRef<ResizeObserver | null>(null);
    const data = segments ?? EMPTY_SEGMENTS;
    const offsets = useMemo(() => {
      const values = new Array<number>(data.length + 1);
      values[0] = 0;
      for (let i = 0; i < data.length; i++) {
        const measured = measurements.heights.get(data[i]);
        values[i + 1] =
          values[i] +
          (measured?.width === viewport.width ? measured.height : 72);
      }
      return values;
    }, [data, measurements, viewport.width]);
    const indexBySegment = useMemo(
      () => new Map(data.map((segment, index) => [segmentKey(segment), index])),
      [data],
    );
    const pinnedFeedbackIndex = useMemo(
      () =>
        pinnedFeedbackId
          ? data.findIndex((segment) => segment.id === pinnedFeedbackId)
          : -1,
      [data, pinnedFeedbackId],
    );
    const previousLayout = useRef({ offsets, data });
    useLayoutEffect(() => {
      const container = containerRef.current;
      const previous = previousLayout.current;
      if (container && previous.offsets.length > 1) {
        const anchorPosition = container.scrollTop - 16;
        let lowIndex = 0;
        let highIndex = Math.min(data.length, previous.offsets.length - 1);
        while (lowIndex < highIndex) {
          const middle = Math.ceil((lowIndex + highIndex) / 2);
          if (previous.offsets[middle] <= anchorPosition) lowIndex = middle;
          else highIndex = middle - 1;
        }
        const anchor = previous.data[lowIndex];
        const newIndex =
          indexBySegment.get(segmentKey(anchor)) ??
          Math.min(lowIndex, data.length);
        const shift =
          (offsets[newIndex] ?? 0) - (previous.offsets[lowIndex] ?? 0);
        if (shift !== 0) container.scrollTop += shift;
      }
      previousLayout.current = { offsets, data };
    }, [data, indexBySegment, offsets]);
    useLayoutEffect(() => {
      if (!pendingFocus) return;
      const row = containerRef.current?.querySelector<HTMLElement>(
        `[data-index="${pendingFocus.index}"]`,
      );
      const button = row?.querySelector<HTMLButtonElement>(
        pendingFocus.control === "feedback"
          ? ".segment-feedback-button"
          : ".segment",
      );
      if (button) {
        button.focus();
        setPendingFocus(null);
      }
    }, [pendingFocus]);
    const low = Math.max(0, viewport.top - 1000);
    const high = viewport.top + viewport.height + 1000;
    let start = 0;
    while (start < data.length && offsets[start + 1] < low) start++;
    let end = start;
    while (end < data.length && offsets[end] < high) end++;
    const visible = new Set<number>();
    for (let i = start; i < end; i++) visible.add(i);
    if (activeIndex >= 0 && activeIndex < data.length) visible.add(activeIndex);
    if (focusedIndex !== null && focusedIndex < data.length)
      visible.add(focusedIndex);
    if (pinnedFeedbackIndex >= 0) visible.add(pinnedFeedbackIndex);
    if (pendingFocus && pendingFocus.index < data.length)
      visible.add(pendingFocus.index);
    const setRow = useCallback(
      (
        index: number,
        segment: TranscriptSegment,
        node: HTMLLIElement | null,
      ) => {
        const previous = rowNodes.current.get(index);
        if (previous) {
          observerRef.current?.unobserve(previous);
          rowSegments.current.delete(previous);
        }
        if (!node) {
          rowNodes.current.delete(index);
          return;
        }
        rowNodes.current.set(index, node);
        rowSegments.current.set(node, segment);
        observerRef.current?.observe(node);
      },
      [],
    );
    useEffect(() => {
      const container = containerRef.current;
      const measure = (observedHeight?: number, observedWidth?: number) => {
        if (container) {
          const style = getComputedStyle(container);
          const contentWidth = Math.max(
            0,
            container.clientWidth -
              Number.parseFloat(style.paddingLeft) -
              Number.parseFloat(style.paddingRight),
          );
          setViewport({
            top: container.scrollTop,
            height: container.clientHeight || observedHeight || 600,
            width: Math.round(contentWidth || observedWidth || 0),
          });
        }
      };
      const handleScroll = () => measure();
      measure();
      container?.addEventListener("scroll", handleScroll, { passive: true });
      if (typeof ResizeObserver === "undefined")
        return () => container?.removeEventListener("scroll", handleScroll);
      observerRef.current = new ResizeObserver((entries) => {
        for (const entry of entries) {
          if (entry.target === container) {
            measure(
              Math.round(entry.contentRect.height),
              Math.round(entry.contentRect.width),
            );
            continue;
          }
          const row = entry.target as HTMLLIElement;
          const index = Number(row.dataset.index);
          const height = entry.contentRect.height;
          const width = Math.round(entry.contentRect.width);
          const segment = rowSegments.current.get(row);
          if (
            segment &&
            rowNodes.current.get(index) === row &&
            height > 0 &&
            (measurementsRef.current.heights.get(segment)?.height !== height ||
              measurementsRef.current.heights.get(segment)?.width !== width)
          ) {
            measurementsRef.current.heights.set(segment, { height, width });
            setMeasurements((current) => ({
              ...current,
              revision: current.revision + 1,
            }));
          }
        }
      });
      if (container) observerRef.current.observe(container);
      for (const node of rowNodes.current.values())
        observerRef.current.observe(node);
      return () => {
        container?.removeEventListener("scroll", handleScroll);
        observerRef.current?.disconnect();
        observerRef.current = null;
      };
    }, []);
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
        <div className="transcript-container" ref={containerRef}>
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
        <div className="transcript-container" ref={containerRef}>
          {statusNode}
          <LoadingSpinner text={"\u8aad\u307f\u8fbc\u307f\u4e2d..."} />
        </div>
      );
    }

    if (segments.length === 0) {
      return (
        <div className="transcript-container" ref={containerRef}>
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
      <div className="transcript-container" ref={containerRef}>
        {statusNode}
        <ol
          className="transcript-window"
          style={{ height: offsets[segments.length] }}
          aria-label="文字起こし"
        >
          {[...visible]
            .sort((a, b) => a - b)
            .map((i) => {
              const seg = segments[i];
              return (
                <li
                  key={segmentKey(seg)}
                  data-index={i}
                  aria-posinset={i + 1}
                  aria-setsize={segments.length}
                  ref={(node) => setRow(i, seg, node)}
                  className="transcript-window-row"
                  style={{ transform: `translateY(${offsets[i]}px)` }}
                  onFocusCapture={() => {
                    setFocusedIndex(i);
                    if (pinnedFeedbackId && seg.id === pinnedFeedbackId)
                      setPinnedFeedbackId(null);
                  }}
                  onKeyDownCapture={(event) => {
                    if (event.key !== "Tab") return;
                    const focusedButton = (event.target as HTMLElement).closest(
                      "button",
                    );
                    if (!focusedButton) return;
                    const isFeedback = focusedButton.classList.contains(
                      "segment-feedback-button",
                    );
                    let nextIndex: number;
                    let control: "segment" | "feedback";
                    if (event.shiftKey) {
                      if (isFeedback) {
                        nextIndex = i;
                        control = "segment";
                      } else {
                        if (i === 0) return;
                        nextIndex = i - 1;
                        control =
                          onFeedback && segments[nextIndex].id
                            ? "feedback"
                            : "segment";
                      }
                    } else if (!isFeedback && onFeedback && seg.id) {
                      nextIndex = i;
                      control = "feedback";
                    } else {
                      if (i === segments.length - 1) return;
                      nextIndex = i + 1;
                      control = "segment";
                    }
                    const buttons = Array.from(
                      containerRef.current?.querySelectorAll<HTMLButtonElement>(
                        "button",
                      ) ?? [],
                    );
                    const currentButtonIndex = buttons.indexOf(focusedButton);
                    const nativeNext =
                      buttons[currentButtonIndex + (event.shiftKey ? -1 : 1)];
                    const nativeRowIndex = Number(
                      nativeNext?.closest<HTMLElement>("[data-index]")?.dataset
                        .index,
                    );
                    const nativeControl = nativeNext?.classList.contains(
                      "segment-feedback-button",
                    )
                      ? "feedback"
                      : "segment";
                    if (
                      nativeRowIndex === nextIndex &&
                      nativeControl === control
                    )
                      return;
                    event.preventDefault();
                    setFocusedIndex(nextIndex);
                    setPendingFocus({ index: nextIndex, control });
                  }}
                  onBlurCapture={(event) => {
                    if (
                      !event.currentTarget.contains(
                        event.relatedTarget as Node | null,
                      )
                    )
                      setFocusedIndex(null);
                  }}
                >
                  <TranscriptSegmentRow
                    segment={seg}
                    isActive={i === activeIndex}
                    onSeek={onSeek}
                    onFeedback={
                      onFeedback
                        ? (segment, returnFocusTo) => {
                            if (segment.id) setPinnedFeedbackId(segment.id);
                            onFeedback(segment, returnFocusTo);
                          }
                        : undefined
                    }
                  />
                </li>
              );
            })}
        </ol>
      </div>
    );
  },
);
