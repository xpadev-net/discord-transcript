import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";
import type { TranscriptSegment } from "../lib/types";
import { TranscriptPanel } from "./TranscriptPanel";

function segment(index: number): TranscriptSegment {
  return {
    id: `segment-${index}`,
    speaker_id: "speaker",
    speaker: {
      id: "speaker",
      username: "Speaker",
      nickname: null,
      display_name: null,
      display_label: null,
    },
    start_ms: index * 1000,
    end_ms: index * 1000 + 900,
    text: `Transcript line ${index}`,
    confidence: 1,
    is_noisy: false,
    source: "voice",
  };
}

describe("TranscriptPanel", () => {
  it("keeps the transcript window bounded and includes the active row", () => {
    const segments = Array.from({ length: 2000 }, (_, index) => segment(index));
    const { container } = render(
      <TranscriptPanel
        segments={segments}
        activeIndex={1500}
        onSeek={vi.fn()}
        error={null}
        onRetry={vi.fn()}
      />,
    );

    expect(screen.getByText("Transcript line 1500")).toBeTruthy();
    expect(
      container.querySelectorAll(".transcript-window-row").length,
    ).toBeLessThan(50);
    expect(container.querySelectorAll(".transcript-window").length).toBe(1);
    const transcriptContainer = container.querySelector<HTMLDivElement>(
      ".transcript-container",
    );
    if (!transcriptContainer)
      throw new Error("Transcript container was not mounted");
    transcriptContainer.scrollTop = 72 * 500;
    fireEvent.scroll(transcriptContainer);
    expect(container.querySelector('[data-index="500"]')).toBeTruthy();
  });

  it("preserves loading, error, empty and live empty states", () => {
    const common = { activeIndex: -1, onSeek: vi.fn(), onRetry: vi.fn() };
    const { rerender } = render(
      <TranscriptPanel {...common} segments={null} error={null} />,
    );
    expect(screen.getByText("読み込み中...")).toBeTruthy();
    rerender(<TranscriptPanel {...common} segments={[]} error="failed" />);
    expect(screen.getByRole("alert")).toBeTruthy();
    rerender(<TranscriptPanel {...common} segments={[]} error={null} />);
    expect(
      screen.getByText("この会議の文字起こしはまだ利用できません"),
    ).toBeTruthy();
    rerender(<TranscriptPanel {...common} segments={[]} error={null} isLive />);
    expect(screen.getByText("文字起こしの到着を待っています")).toBeTruthy();
  });

  it("does not reuse a measured row height for a replacement transcript item", async () => {
    const observers: Array<{
      callback: ResizeObserverCallback;
      nodes: Set<Element>;
    }> = [];
    class TestResizeObserver {
      nodes = new Set<Element>();
      callback: ResizeObserverCallback;
      constructor(callback: ResizeObserverCallback) {
        this.callback = callback;
        observers.push(this);
      }
      observe(node: Element) {
        this.nodes.add(node);
      }
      unobserve(node: Element) {
        this.nodes.delete(node);
      }
      disconnect() {
        this.nodes.clear();
      }
    }
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      const ref = createRef<HTMLDivElement>();
      const initial = [segment(1), segment(2)];
      const { container, rerender } = render(
        <TranscriptPanel
          ref={ref}
          segments={initial}
          activeIndex={-1}
          onSeek={vi.fn()}
          error={null}
          onRetry={vi.fn()}
        />,
      );
      const firstObserver = observers.at(-1);
      const firstRow = container.querySelector('[data-index="0"]');
      const transcriptContainer = container.querySelector(
        ".transcript-container",
      );
      expect(firstObserver).toBeTruthy();
      expect(firstRow).toBeTruthy();
      expect(transcriptContainer).toBeTruthy();
      act(() => {
        firstObserver?.callback(
          [
            {
              target: transcriptContainer,
              contentRect: { height: 600, width: 800 },
            } as unknown as ResizeObserverEntry,
          ],
          firstObserver as unknown as ResizeObserver,
        );
      });
      const measuredRow = container.querySelector('[data-index="0"]');
      act(() => {
        firstObserver?.callback(
          [
            {
              target: measuredRow,
              contentRect: { height: 180, width: 800 },
            } as ResizeObserverEntry,
          ],
          firstObserver as unknown as ResizeObserver,
        );
      });
      await waitFor(() =>
        expect(
          container.querySelector(".transcript-window")?.getAttribute("style"),
        ).toContain("252px"),
      );

      act(() => {
        firstObserver?.callback(
          [
            {
              target: transcriptContainer,
              contentRect: { height: 600, width: 400 },
            } as ResizeObserverEntry,
          ],
          firstObserver as unknown as ResizeObserver,
        );
      });
      await waitFor(() =>
        expect(
          container.querySelector(".transcript-window")?.getAttribute("style"),
        ).toContain("144px"),
      );

      rerender(
        <TranscriptPanel
          ref={ref}
          segments={[segment(10), segment(11)]}
          activeIndex={-1}
          onSeek={vi.fn()}
          error={null}
          onRetry={vi.fn()}
        />,
      );
      await waitFor(() =>
        expect(
          container.querySelector(".transcript-window")?.getAttribute("style"),
        ).toContain("144px"),
      );

      const large = Array.from({ length: 2000 }, (_, index) => segment(index));
      rerender(
        <TranscriptPanel
          ref={ref}
          segments={large}
          activeIndex={-1}
          onSeek={vi.fn()}
          error={null}
          onRetry={vi.fn()}
        />,
      );
      const rowsBeforeResize = container.querySelectorAll(
        ".transcript-window-row",
      ).length;
      const largeObserver = observers.at(-1);
      act(() => {
        largeObserver?.callback(
          [
            {
              target: transcriptContainer,
              contentRect: { height: 5000, width: 800 },
            } as ResizeObserverEntry,
          ],
          largeObserver as unknown as ResizeObserver,
        );
      });
      await waitFor(() =>
        expect(
          container.querySelectorAll(".transcript-window-row").length,
        ).toBeGreaterThan(rowsBeforeResize),
      );
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("keeps the same segment at the scroll anchor when an earlier row is inserted", () => {
    const segments = Array.from({ length: 200 }, (_, index) => segment(index));
    const ref = createRef<HTMLDivElement>();
    const { rerender } = render(
      <TranscriptPanel
        ref={ref}
        segments={segments}
        activeIndex={-1}
        onSeek={vi.fn()}
        error={null}
        onRetry={vi.fn()}
      />,
    );
    const transcriptContainer = ref.current;
    if (!transcriptContainer)
      throw new Error("Transcript container was not mounted");
    transcriptContainer.scrollTop = 16 + 72 * 100;
    fireEvent.scroll(transcriptContainer);
    const previousScrollTop = transcriptContainer.scrollTop;

    rerender(
      <TranscriptPanel
        ref={ref}
        segments={[segment(-1), ...segments]}
        activeIndex={-1}
        onSeek={vi.fn()}
        error={null}
        onRetry={vi.fn()}
      />,
    );

    expect(transcriptContainer.scrollTop).toBe(previousScrollTop + 72);

    const anchor = { ...segments[100], start_ms: -100, end_ms: 50 };
    const before = transcriptContainer.scrollTop;
    rerender(
      <TranscriptPanel
        ref={ref}
        segments={[
          anchor,
          segment(-1),
          ...segments.slice(0, 100),
          ...segments.slice(101),
        ]}
        activeIndex={-1}
        onSeek={vi.fn()}
        error={null}
        onRetry={vi.fn()}
      />,
    );
    expect(transcriptContainer.scrollTop).toBe(before - 72 * 101);
  });

  it("associates queued resize entries with the row's current segment identity", async () => {
    const observers: ResizeObserverCallback[] = [];
    class TestResizeObserver {
      constructor(callback: ResizeObserverCallback) {
        observers.push(callback);
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      const ref = createRef<HTMLDivElement>();
      const segments = [segment(1), segment(2)];
      const { container, rerender } = render(
        <TranscriptPanel
          ref={ref}
          segments={segments}
          activeIndex={-1}
          onSeek={vi.fn()}
          error={null}
          onRetry={vi.fn()}
        />,
      );
      const oldObserver = observers[0];
      const containerNode = ref.current;
      const originalFirstRow = container.querySelector('[data-index="0"]');
      if (!oldObserver || !containerNode || !originalFirstRow)
        throw new Error("Resize observer was not attached");
      act(() => {
        oldObserver(
          [
            {
              target: containerNode,
              contentRect: { height: 600, width: 800 },
            } as unknown as ResizeObserverEntry,
          ],
          {} as ResizeObserver,
        );
      });

      rerender(
        <TranscriptPanel
          ref={ref}
          segments={[segments[1], segments[0]]}
          activeIndex={-1}
          onSeek={vi.fn()}
          error={null}
          onRetry={vi.fn()}
        />,
      );
      const movedFirstRow = container.querySelector('[data-index="1"]');
      if (!movedFirstRow)
        throw new Error("Moved transcript row was not mounted");
      act(() => {
        oldObserver(
          [
            {
              target: movedFirstRow,
              contentRect: { height: 180, width: 800 },
            } as ResizeObserverEntry,
          ],
          {} as ResizeObserver,
        );
      });

      await waitFor(() =>
        expect(
          container.querySelector('[data-index="1"]')?.getAttribute("style"),
        ).toContain("72px"),
      );
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it.each([
    { shift: false, focused: 10, scrollTo: 1000, expected: 11 },
    { shift: true, focused: 1000, scrollTo: 10, expected: 999 },
  ])("moves keyboard focus to the adjacent row after distant scrolling", async ({
    shift,
    focused,
    scrollTo,
    expected,
  }) => {
    const ref = createRef<HTMLDivElement>();
    const { container } = render(
      <TranscriptPanel
        ref={ref}
        segments={Array.from({ length: 2000 }, (_, index) => segment(index))}
        activeIndex={focused}
        onSeek={vi.fn()}
        error={null}
        onRetry={vi.fn()}
      />,
    );
    const focusedButton = container.querySelector<HTMLButtonElement>(
      `[data-index="${focused}"] .segment`,
    );
    const transcriptContainer = ref.current;
    if (!focusedButton || !transcriptContainer)
      throw new Error("Transcript row was not mounted");
    focusedButton.focus();
    transcriptContainer.scrollTop = scrollTo * 72;
    fireEvent.scroll(transcriptContainer);
    fireEvent.keyDown(focusedButton, { key: "Tab", shiftKey: shift });

    await waitFor(() => {
      const expectedButton = container.querySelector(
        `[data-index="${expected}"] .segment`,
      );
      expect(expectedButton).toBeTruthy();
      expect(document.activeElement).toBe(expectedButton);
    });
  });

  it("keeps the feedback row mounted while focus is in the dialog", () => {
    const segments = Array.from({ length: 2000 }, (_, index) => segment(index));
    const onFeedback = vi.fn();
    const { container } = render(
      <TranscriptPanel
        segments={segments}
        activeIndex={-1}
        onSeek={vi.fn()}
        onFeedback={onFeedback}
        error={null}
        onRetry={vi.fn()}
      />,
    );
    const feedbackButton = container.querySelector<HTMLButtonElement>(
      '[data-index="10"] .segment-feedback-button',
    );
    const transcriptContainer = container.querySelector<HTMLDivElement>(
      ".transcript-container",
    );
    if (!feedbackButton || !transcriptContainer)
      throw new Error("Feedback row was not mounted");
    const dialogControl = document.createElement("button");
    document.body.appendChild(dialogControl);
    feedbackButton.focus();
    fireEvent.click(feedbackButton);
    dialogControl.focus();
    transcriptContainer.scrollTop = 72 * 1000;
    fireEvent.scroll(transcriptContainer);

    expect(onFeedback).toHaveBeenCalledOnce();
    expect(container.querySelector('[data-index="10"]')).toBeTruthy();
    dialogControl.remove();
  });
});
