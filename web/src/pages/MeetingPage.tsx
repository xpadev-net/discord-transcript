import {
  type FormEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { useParams } from "react-router-dom";
import { AudioPlayer } from "../components/AudioPlayer";
import { DebugDownloads } from "../components/DebugDownloads";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { Header } from "../components/Header";
import { SummaryPanel } from "../components/SummaryPanel";
import { TranscriptPanel } from "../components/TranscriptPanel";
import { useAudioSync } from "../hooks/useAudioSync";
import { useMeetingData } from "../hooks/useMeetingData";
import {
  createMeetingFeedback,
  fetchDebugManifest,
  getAudioUrl,
} from "../lib/api";
import { isLiveMeetingStatus } from "../lib/meetingStatus";
import type {
  DebugArtifact,
  TranscriptFeedbackRequest,
  TranscriptFeedbackTermType,
  TranscriptFeedbackType,
  TranscriptSegment,
} from "../lib/types";

const feedbackTypeOptions: Array<{
  value: TranscriptFeedbackType;
  label: string;
}> = [
  { value: "mistranscription", label: "文字起こし" },
  { value: "speaker", label: "話者" },
  { value: "term", label: "用語" },
];

const termTypeOptions: Array<{
  value: TranscriptFeedbackTermType;
  label: string;
}> = [
  { value: "general_term", label: "一般用語" },
  { value: "person_name", label: "人名" },
  { value: "project_name", label: "プロジェクト名" },
  { value: "product_name", label: "製品名" },
  { value: "organization", label: "組織名" },
  { value: "acronym", label: "略語" },
  { value: "wording_rule", label: "表記ルール" },
  { value: "prohibited_item", label: "禁止語" },
];

const INITIAL_FEEDBACK_FOCUS_SELECTOR =
  "select:not(:disabled), textarea:not(:disabled), input:not(:disabled)";

const FOCUSABLE_SELECTOR =
  'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [href], [tabindex]:not([tabindex="-1"])';

interface FeedbackDraft {
  feedbackType: TranscriptFeedbackType;
  correctedText: string;
  correctedSpeakerId: string;
  termType: TranscriptFeedbackTermType;
  note: string;
}

function emptyFeedbackDraft(segment: TranscriptSegment): FeedbackDraft {
  return {
    feedbackType: "mistranscription",
    correctedText: segment.text,
    correctedSpeakerId: "",
    termType: "general_term",
    note: "",
  };
}

function segmentSpeakerId(segment: TranscriptSegment): string {
  return segment.speaker?.id || segment.speaker_id;
}

function optionalText(value: string): string | undefined {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function buildFeedbackRequest(
  segment: TranscriptSegment,
  draft: FeedbackDraft,
): TranscriptFeedbackRequest {
  const request: TranscriptFeedbackRequest = {
    transcript_segment_id: segment.id,
    feedback_type: draft.feedbackType,
    original_text: segment.text,
    note: optionalText(draft.note),
  };

  if (draft.feedbackType === "mistranscription") {
    request.corrected_text = optionalText(draft.correctedText);
  }
  if (draft.feedbackType === "speaker") {
    request.speaker_id = segmentSpeakerId(segment);
    request.corrected_speaker_id = optionalText(draft.correctedSpeakerId);
  }
  if (draft.feedbackType === "term") {
    request.term_type = draft.termType;
    request.corrected_text = optionalText(draft.correctedText);
  }

  return request;
}

function feedbackFocusableElements(dialog: HTMLElement): HTMLElement[] {
  return Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
}

function validateFeedbackDraft(
  segment: TranscriptSegment,
  draft: FeedbackDraft,
): string | null {
  if (
    draft.feedbackType === "mistranscription" ||
    draft.feedbackType === "term"
  ) {
    if (!draft.correctedText.trim()) {
      return "修正後の文字起こしを入力してください";
    }
    if (draft.correctedText.trim() === segment.text.trim()) {
      return "修正後の文字起こしを元のテキストから変更してください";
    }
  }
  if (draft.feedbackType === "speaker" && !draft.correctedSpeakerId.trim()) {
    return "正しい話者IDまたは名前を入力してください";
  }
  return null;
}

function feedbackSubmitErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message.startsWith("400")) {
    return "入力内容がサーバーの検証に通りませんでした";
  }
  if (error instanceof Error && error.message.startsWith("403")) {
    return "この会議にフィードバックを送信する権限がありません";
  }
  return "フィードバックの送信に失敗しました";
}

interface FeedbackDialogProps {
  segment: TranscriptSegment;
  draft: FeedbackDraft;
  submitting: boolean;
  error: string | null;
  onDraftChange: (draft: FeedbackDraft) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onClose: () => void;
}

function FeedbackDialog({
  segment,
  draft,
  submitting,
  error,
  onDraftChange,
  onSubmit,
  onClose,
}: FeedbackDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const firstField = dialogRef.current?.querySelector<HTMLElement>(
      INITIAL_FEEDBACK_FOCUS_SELECTOR,
    );
    firstField?.focus();
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: globalThis.KeyboardEvent) => {
      if (!dialogRef.current?.contains(document.activeElement)) {
        return;
      }

      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key !== "Tab") {
        return;
      }

      const focusable = dialogRef.current
        ? feedbackFocusableElements(dialogRef.current)
        : [];
      if (focusable.length === 0) {
        event.preventDefault();
        return;
      }

      const activeIndex = focusable.indexOf(
        document.activeElement as HTMLElement,
      );
      const nextIndex = event.shiftKey
        ? activeIndex <= 0
          ? focusable.length - 1
          : activeIndex - 1
        : activeIndex < 0 || activeIndex === focusable.length - 1
          ? 0
          : activeIndex + 1;
      event.preventDefault();
      focusable[nextIndex].focus();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  return (
    <div className="feedback-modal-backdrop" role="presentation">
      <dialog
        ref={dialogRef}
        open
        className="feedback-modal"
        aria-modal="true"
        aria-labelledby="feedback-dialog-title"
      >
        <div className="feedback-modal-header">
          <div>
            <h2 id="feedback-dialog-title">フィードバック</h2>
            <p>{segment.text}</p>
          </div>
          <button
            type="button"
            className="feedback-close-button"
            onClick={onClose}
            aria-label="フィードバックを閉じる"
            disabled={submitting}
          >
            ×
          </button>
        </div>
        <form className="feedback-form" onSubmit={onSubmit}>
          <label className="feedback-field">
            <span>種類</span>
            <select
              value={draft.feedbackType}
              onChange={(event) =>
                onDraftChange({
                  ...draft,
                  feedbackType: event.target.value as TranscriptFeedbackType,
                })
              }
              disabled={submitting}
            >
              {feedbackTypeOptions.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          {draft.feedbackType !== "speaker" ? (
            <label className="feedback-field">
              <span>修正後の文字起こし</span>
              <textarea
                value={draft.correctedText}
                rows={4}
                onChange={(event) =>
                  onDraftChange({ ...draft, correctedText: event.target.value })
                }
                disabled={submitting}
              />
            </label>
          ) : null}
          {draft.feedbackType === "speaker" ? (
            <label className="feedback-field">
              <span>正しい話者IDまたは名前</span>
              <input
                type="text"
                value={draft.correctedSpeakerId}
                placeholder={segmentSpeakerId(segment)}
                onChange={(event) =>
                  onDraftChange({
                    ...draft,
                    correctedSpeakerId: event.target.value,
                  })
                }
                disabled={submitting}
              />
            </label>
          ) : null}
          {draft.feedbackType === "term" ? (
            <label className="feedback-field">
              <span>用語タイプ</span>
              <select
                value={draft.termType}
                onChange={(event) =>
                  onDraftChange({
                    ...draft,
                    termType: event.target.value as TranscriptFeedbackTermType,
                  })
                }
                disabled={submitting}
              >
                {termTypeOptions.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </label>
          ) : null}
          <label className="feedback-field">
            <span>メモ・ヒント</span>
            <textarea
              value={draft.note}
              rows={3}
              onChange={(event) =>
                onDraftChange({ ...draft, note: event.target.value })
              }
              disabled={submitting}
            />
          </label>
          {error ? (
            <div className="feedback-error" role="alert">
              {error}
            </div>
          ) : null}
          <div className="feedback-actions">
            <button
              type="button"
              className="secondary-button"
              onClick={onClose}
              disabled={submitting}
            >
              キャンセル
            </button>
            <button
              type="submit"
              className="primary-button"
              disabled={submitting}
            >
              {submitting ? "送信中..." : "送信"}
            </button>
          </div>
        </form>
      </dialog>
    </div>
  );
}

function inProgressMessage(status: string | undefined): string | null {
  if (status === "recording") {
    return "\u9332\u97f3\u4e2d\u3067\u3059\u3002\u6587\u5b57\u8d77\u3053\u3057\u304c\u5230\u7740\u3059\u308b\u3068\u3053\u306e\u30da\u30fc\u30b8\u306b\u8ffd\u52a0\u3055\u308c\u307e\u3059\u3002";
  }
  if (status === "stopping") {
    return "\u9332\u97f3\u306e\u505c\u6b62\u51e6\u7406\u4e2d\u3067\u3059\u3002\u97f3\u58f0\u3068\u6587\u5b57\u8d77\u3053\u3057\u3092\u6e96\u5099\u3057\u3066\u3044\u307e\u3059\u3002";
  }
  if (status === "transcribing" || status === "processing") {
    return "\u6587\u5b57\u8d77\u3053\u3057\u4e2d\u3067\u3059\u3002\u7d50\u679c\u306f\u9806\u6b21\u8ffd\u52a0\u3055\u308c\u307e\u3059\u3002";
  }
  if (status === "summarizing") {
    return "\u8981\u7d04\u4e2d\u3067\u3059\u3002\u6587\u5b57\u8d77\u3053\u3057\u306f\u5f15\u304d\u7d9a\u304d\u78ba\u8a8d\u3067\u304d\u307e\u3059\u3002";
  }
  return null;
}

export function MeetingPage() {
  const { meetingId } = useParams<{ meetingId: string }>();
  const audioRef = useRef<HTMLAudioElement>(null);
  const transcriptContainerRef = useRef<HTMLDivElement>(null);
  const feedbackSubmitControllerRef = useRef<AbortController | null>(null);
  const feedbackReturnFocusRef = useRef<HTMLElement | null>(null);
  const feedbackSubmitFocusRef = useRef<HTMLElement | null>(null);
  const feedbackSuccessTimeoutRef = useRef<number | null>(null);

  const {
    meeting,
    transcript,
    summary,
    error,
    transcriptError,
    summaryError,
    transcriptStreamState,
    transcriptStreamError,
    retryTranscript,
    retrySummary,
  } = useMeetingData(meetingId);
  const { activeIndex, seekTo, seekNotice } = useAudioSync(
    audioRef,
    transcriptContainerRef,
    transcript,
  );

  const [debugArtifacts, setDebugArtifacts] = useState<DebugArtifact[] | null>(
    null,
  );
  const [debugLoading, setDebugLoading] = useState(true);
  const [debugError, setDebugError] = useState(false);
  const [feedbackSegment, setFeedbackSegment] =
    useState<TranscriptSegment | null>(null);
  const [feedbackDraft, setFeedbackDraft] = useState<FeedbackDraft | null>(
    null,
  );
  const [feedbackSubmitting, setFeedbackSubmitting] = useState(false);
  const [feedbackError, setFeedbackError] = useState<string | null>(null);
  const [feedbackSuccess, setFeedbackSuccess] = useState<string | null>(null);
  const isLive = isLiveMeetingStatus(meeting?.status);
  const progressMessage = inProgressMessage(meeting?.status);
  const showAudioAndDebug = meeting?.status === "posted";

  useEffect(() => {
    if (meetingId) {
      document.title = meeting?.title || "Meeting";
    }
  }, [meetingId, meeting?.title]);

  useEffect(() => {
    if (!meetingId || !showAudioAndDebug) {
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
  }, [meetingId, showAudioAndDebug]);

  const clearFeedbackSuccessTimer = useCallback(() => {
    if (feedbackSuccessTimeoutRef.current !== null) {
      window.clearTimeout(feedbackSuccessTimeoutRef.current);
      feedbackSuccessTimeoutRef.current = null;
    }
  }, []);

  const showFeedbackSuccess = useCallback(
    (message: string) => {
      clearFeedbackSuccessTimer();
      setFeedbackSuccess(message);
      feedbackSuccessTimeoutRef.current = window.setTimeout(() => {
        setFeedbackSuccess(null);
        feedbackSuccessTimeoutRef.current = null;
      }, 4000);
    },
    [clearFeedbackSuccessTimer],
  );

  useEffect(() => {
    if (!meetingId) {
      clearFeedbackSuccessTimer();
      setFeedbackSuccess(null);
      return;
    }
    feedbackSubmitControllerRef.current?.abort();
    clearFeedbackSuccessTimer();
    setFeedbackSegment(null);
    setFeedbackDraft(null);
    setFeedbackSubmitting(false);
    setFeedbackError(null);
    setFeedbackSuccess(null);
    return () => {
      feedbackSubmitControllerRef.current?.abort();
      clearFeedbackSuccessTimer();
    };
  }, [meetingId, clearFeedbackSuccessTimer]);

  const openFeedback = useCallback(
    (segment: TranscriptSegment, returnFocusTo: HTMLElement) => {
      feedbackReturnFocusRef.current = returnFocusTo;
      setFeedbackSegment(segment);
      setFeedbackDraft(emptyFeedbackDraft(segment));
      setFeedbackError(null);
      clearFeedbackSuccessTimer();
      setFeedbackSuccess(null);
    },
    [clearFeedbackSuccessTimer],
  );

  const restoreFeedbackFocus = useCallback(() => {
    window.setTimeout(() => {
      feedbackReturnFocusRef.current?.focus();
      feedbackReturnFocusRef.current = null;
    }, 0);
  }, []);

  const closeFeedback = useCallback(() => {
    if (feedbackSubmitting) {
      return;
    }
    setFeedbackSegment(null);
    setFeedbackDraft(null);
    setFeedbackError(null);
    restoreFeedbackFocus();
  }, [feedbackSubmitting, restoreFeedbackFocus]);

  const submitFeedback = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      if (!meetingId || !feedbackSegment || !feedbackDraft) {
        return;
      }
      const validationError = validateFeedbackDraft(
        feedbackSegment,
        feedbackDraft,
      );
      if (validationError) {
        setFeedbackError(validationError);
        return;
      }

      feedbackSubmitFocusRef.current =
        document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null;
      setFeedbackSubmitting(true);
      setFeedbackError(null);
      feedbackSubmitControllerRef.current?.abort();
      const controller = new AbortController();
      feedbackSubmitControllerRef.current = controller;
      createMeetingFeedback(
        meetingId,
        buildFeedbackRequest(feedbackSegment, feedbackDraft),
        controller.signal,
      )
        .then(() => {
          if (controller.signal.aborted) {
            return;
          }
          showFeedbackSuccess("フィードバックを送信しました");
          setFeedbackSegment(null);
          setFeedbackDraft(null);
          restoreFeedbackFocus();
        })
        .catch((err: unknown) => {
          if (controller.signal.aborted) {
            return;
          }
          setFeedbackError(feedbackSubmitErrorMessage(err));
          window.setTimeout(() => {
            feedbackSubmitFocusRef.current?.focus();
            feedbackSubmitFocusRef.current = null;
          }, 0);
        })
        .finally(() => {
          if (feedbackSubmitControllerRef.current === controller) {
            feedbackSubmitControllerRef.current = null;
            setFeedbackSubmitting(false);
          }
        });
    },
    [
      feedbackDraft,
      feedbackSegment,
      meetingId,
      restoreFeedbackFocus,
      showFeedbackSuccess,
    ],
  );

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
          {progressMessage ? (
            <div className="meeting-progress-notice" role="status">
              {progressMessage}
            </div>
          ) : null}
          {showAudioAndDebug ? (
            <AudioPlayer
              key={meetingId}
              ref={audioRef}
              src={meetingId ? getAudioUrl(meetingId) : ""}
            />
          ) : (
            <div className="audio-container audio-placeholder" role="status">
              {
                "\u97f3\u58f0\u30d7\u30ec\u30fc\u30e4\u30fc\u306f\u9332\u97f3\u7d42\u4e86\u5f8c\u306b\u5229\u7528\u3067\u304d\u307e\u3059"
              }
            </div>
          )}
          {meetingId && showAudioAndDebug ? (
            <DebugDownloads
              artifacts={debugArtifacts}
              loading={debugLoading}
              error={debugError}
            />
          ) : null}
          {seekNotice ? (
            <div className="seek-notice" role="status">
              {seekNotice}
            </div>
          ) : null}
          {feedbackSuccess ? (
            <div className="feedback-success" role="status">
              {feedbackSuccess}
            </div>
          ) : null}
          <ErrorBoundary
            title={
              "\u30c8\u30e9\u30f3\u30b9\u30af\u30ea\u30d7\u30c8\u306e\u8868\u793a\u306b\u5931\u6557\u3057\u307e\u3057\u305f"
            }
          >
            <TranscriptPanel
              ref={transcriptContainerRef}
              segments={transcript}
              activeIndex={activeIndex}
              onSeek={seekTo}
              error={transcriptError}
              onRetry={retryTranscript}
              onFeedback={openFeedback}
              streamState={transcriptStreamState}
              streamError={transcriptStreamError}
              isLive={isLive}
            />
          </ErrorBoundary>
        </div>
        <ErrorBoundary
          title={
            "\u30b5\u30de\u30ea\u30fc\u306e\u8868\u793a\u306b\u5931\u6557\u3057\u307e\u3057\u305f"
          }
        >
          <SummaryPanel
            markdown={summary?.markdown}
            loading={
              !isLive &&
              !!meetingId &&
              summary === null &&
              summaryError === null
            }
            error={summaryError}
            onRetry={retrySummary}
          />
        </ErrorBoundary>
      </div>
      {feedbackSegment && feedbackDraft ? (
        <FeedbackDialog
          segment={feedbackSegment}
          draft={feedbackDraft}
          submitting={feedbackSubmitting}
          error={feedbackError}
          onDraftChange={setFeedbackDraft}
          onSubmit={submitFeedback}
          onClose={closeFeedback}
        />
      ) : null}
    </>
  );
}
