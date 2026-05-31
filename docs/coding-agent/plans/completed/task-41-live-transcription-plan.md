# Task 41 Live Transcription Plan

## Context
- Objective: implement Task 41, "音声を随時文字起こしする", through PR, review hook, and merge.
- User waived plan approval by explicitly asking this thread to execute through merge.
- Branch: `codex/task-41-live-transcription`.
- Repo rule suite: `docs/coding-agent/rules/` is absent in this worktree; use harness and engineering baselines.
- Quality routing: L2. In scope: Rust async/runtime/data integrity, SQL contract, TypeScript API/UI data flow. Top risks: data integrity, concurrency/ordering, external ASR retries, API contract compatibility, Task 42 conflict hotspots.

## Task_1
- type: impl
- owns:
  - `src/audio/receiver.rs`
  - `src/audio/recorder.rs`
  - `src/audio/recording_session.rs`
  - `src/application/runtime.rs`
- depends_on: []
- acceptance:
  - Recording chunks flush at about one minute while speech continues.
  - Buffered speech flushes early after about thirty seconds of silence.
  - Persisted chunks are handed to live ASR outside the Songbird/session lock.
  - Live ASR writes deterministic, retry-safe transcript rows without blocking audio ingest.
- validation:
  - required: true
    owner: worker
    kind: command
    detail: `rtk cargo test --workspace --all-targets --all-features`
  - required: true
    owner: worker
    kind: command
    detail: `rtk cargo fmt --all -- --check`

## Task_2
- type: impl
- owns:
  - `src/application/runtime.rs`
  - `src/audio/meeting_audio.rs`
  - `src/infrastructure/sql.rs`
  - `migrations/*.sql`
- depends_on: [Task_1]
- acceptance:
  - Saved live transcript rows are marked distinctly from final rows.
  - Final summary prefers saved live rows and only transcribes missing voice intervals.
  - Final transcript persistence replaces live rows with final rows cleanly.
  - Failed live chunks remain retryable by live retry and final fallback.
- validation:
  - required: true
    owner: worker
    kind: command
    detail: `rtk cargo test --workspace --all-targets --all-features`
  - required: true
    owner: worker
    kind: command
    detail: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`

## Task_3
- type: impl
- owns:
  - `src/interfaces/web.rs`
  - `web/src/lib/types.ts`
  - `web/src/lib/api.ts`
  - `web/src/hooks/useMeetingData.ts`
  - `web/src/components/TranscriptPanel.tsx`
  - `web/src/pages/MeetingPage.tsx`
- depends_on: [Task_2]
- acceptance:
  - Transcript API exposes minimal metadata for live/final state and last update.
  - Browser can observe in-progress transcript content without SSE.
  - Existing transcript rendering remains compatible with final meetings.
  - Changes in Task 42 hotspot files are minimal and documented for PR.
- validation:
  - required: true
    owner: worker
    kind: command
    detail: `rtk pnpm --dir web run lint`
  - required: true
    owner: worker
    kind: command
    detail: `rtk pnpm --dir web exec tsc --noEmit`
  - required: true
    owner: reviewer
    kind: review
    detail: Independent subagent review of API/UI contract and conflict risk.

## Task_4
- type: review
- owns:
  - `*`
- depends_on: [Task_1, Task_2, Task_3]
- acceptance:
  - Subagent review returns APPROVED or all findings are fixed and re-reviewed.
  - PR is created from `codex/task-41-live-transcription`.
  - `gh-review-hook` exits 0.
  - PR is merged.
- validation:
  - required: true
    owner: reviewer
    kind: review
    detail: Harness reviewer subagent.
  - required: true
    owner: orchestrator
    kind: command
    detail: `rtk gh-review-hook`
  - required: true
    owner: orchestrator
    kind: command
    detail: PR merge command.

## Task Waves
- Wave 1: Task_1
- Wave 2: Task_2
- Wave 3: Task_3
- Wave 4: Task_4

## Progress Log
- Created plan after Researcher returned architecture and risk notes.
- Implemented live chunk flushing, live ASR persistence, final fallback, and minimal transcript state API.
- Ran independent reviewer. Fixed stale live rows, late live writes, timeline rebasing order, and final-row authority issues.
- Reviewer returned APPROVED after re-review.
- Validation passed: Rust fmt, Rust tests, Rust clippy, web lint, web typecheck, web build.

## Decision Log
- Plan approval explicitly waived because user asked for implementation through merge.
- Repo rule suite absent; validation selection derived from harness baselines and local CI/package files.
- To reduce Task 42 conflicts, kept `/api/meetings/{meeting_id}/transcript` as the existing array response and added `/api/meetings/{meeting_id}/transcript/state` for minimal live/final metadata.
