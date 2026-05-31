# Plan: Task 42 Live Meeting Page

- status: in_progress
- generated: 2026-05-31
- last_updated: 2026-05-31
- work_type: code

## Goal

- Users can open a meeting page from the Discord status message and dashboard while a meeting is still recording or transcribing.
- The meeting page displays in-progress status, live transcript updates via SSE, reconnection state, and permission/error states without assuming final audio/summary artifacts exist.

## Definition of Done

- Discord status messages include a meeting URL when `PUBLIC_BASE_URL` is configured during recording/transcribing/completion.
- `/meetings/:id` renders useful in-progress UI for `recording`, `stopping`, `transcribing`, and `summarizing`.
- A protected SSE transcript endpoint streams existing and newly persisted transcript rows from the current `transcripts` table without depending on Task 41-only schema.
- Frontend merges streamed transcript segments without duplicates, reconnects with backoff, and surfaces authorization/connection errors accessibly.
- Required checks pass, independent Reviewer reports no blocking findings, PR is created, `gh-review-hook` exits 0, and the PR is merged.

## Scope / Non-goals

- Scope:
  - `src/interfaces/web.rs`
  - `src/application/runtime.rs`
  - `web/src/lib/api.ts`
  - `web/src/lib/types.ts`
  - `web/src/hooks/useMeetingData.ts`
  - `web/src/pages/MeetingPage.tsx`
  - `web/src/components/Header.tsx`
  - `web/src/components/TranscriptPanel.tsx`
  - `web/src/index.css`
  - focused tests for changed Rust/frontend behavior where repository facilities exist
- Non-goals:
  - Do not implement Task 41 ASR chunking/persistence.
  - Do not add migrations or alter the `transcripts` schema unless Task 41 public API requires it.
  - Avoid broad changes in `src/application/worker.rs`, `src/infrastructure/asr.rs`, and `src/infrastructure/sql.rs`.

## Context (workspace)

- Related files/areas:
  - `src/application/runtime.rs` status message updates and `PUBLIC_BASE_URL` support.
  - `src/interfaces/web.rs` authenticated meeting/transcript APIs and channel permission checks.
  - `web/src/hooks/useMeetingData.ts` currently fetches transcript once and has no live mode.
  - `web/src/pages/MeetingPage.tsx` currently renders final-artifact panels for every status.
- Existing patterns or references:
  - `api_transcript` already returns ordered transcript rows with speaker metadata.
  - `verify_meeting_access` already enforces guild/channel view permissions for protected meeting endpoints.
  - Dashboard rows already link to `/meetings/:id`.
- Repo reference docs consulted:
  - `AGENTS.md`
  - `/Users/xpadev/.codex/RTK.md`
  - `$orchestration-harness`
  - `$plan-format`
  - `$engineering-quality-baselines`
  - `$subagent-strategy`
  - `$improvement-loop`
  - `.github/workflows/ci.yml`

## Open Questions (max 3)

- Q1: None blocking. Task 41 public API/DB shape is not yet available in this worktree.

## Assumptions

- A1: Until Task 41 lands, live transcript segments are represented by rows in the existing `transcripts` table; frontend parsing remains compatible with both the old transcript array and Task 41's planned `{ segments, status, is_final, updated_at }` response.
- A2: SSE can poll the current DB at a modest interval and emit only rows beyond the last emitted tuple.
- A3: User approval for this plan is covered by the delegated task request to proceed through merge.

## Tasks

### Task_1: Research and Design Boundary

- type: research
- owns:
  - docs/coding-agent/plans/active/task-42-live-meeting-page-plan.md
  - docs/coding-agent/lessons.md
- depends_on: []
- description: |
  Confirm Task 42 scope, Task 41 dependency boundary, validation mapping, and plan assumptions.
- acceptance:
  - Task 41 branch/API dependency is checked and documented.
  - Validation commands are inferred from CI and package scripts.
  - User correction about quality-over-speed is recorded in lessons.
- validation:
  - kind: review
    required: true
    owner: orchestrator
    detail: "Plan documents Task 41 dependency boundary and validation commands."

### Task_2: Backend Live Transcript and Status Message URL

- type: impl
- owns:
  - src/interfaces/web.rs
  - src/application/runtime.rs
  - tests/application/runtime_and_worker.rs
- depends_on: [Task_1]
- description: |
  Add a protected SSE endpoint that streams transcript segments from existing DB rows and update Discord status messages to include meeting URLs when configured.
- acceptance:
  - `GET /api/meetings/{meeting_id}/transcript/events` is auth-protected and uses existing meeting access checks.
  - SSE emits current transcript rows and later appended rows without duplicate emissions in one connection.
  - Status messages for recording start, stop/transcribing, summary start, completion, and failure include the meeting URL when available.
  - Backend changes do not require Task 41 schema changes.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "cargo fmt --all -- --check"
  - kind: command
    required: true
    owner: orchestrator
    detail: "cargo test status_message_tests"
  - kind: command
    required: true
    owner: orchestrator
    detail: "cargo test transcript"

### Task_3: Frontend Live Meeting UX

- type: impl
- owns:
  - web/src/lib/api.ts
  - web/src/lib/types.ts
  - web/src/hooks/useMeetingData.ts
  - web/src/pages/MeetingPage.tsx
  - web/src/components/Header.tsx
  - web/src/components/TranscriptPanel.tsx
  - web/src/index.css
- depends_on: [Task_2]
- description: |
  Connect MeetingPage to the live transcript stream, merge updates, expose reconnect/error states, and adapt in-progress artifact panels.
- acceptance:
  - Recording/transcribing pages show a clear live status and do not present final summary/audio as failed just because artifacts are not ready.
  - Transcript panel shows loading, empty-live, connected, reconnecting, and permission/error states with accessible live regions.
  - SSE reconnects with backoff and deduplicates repeated segments.
  - Dashboard links remain keyboard-accessible and status labels cover lifecycle states.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "cd web && pnpm run lint"
  - kind: command
    required: true
    owner: orchestrator
    detail: "cd web && pnpm exec tsc --noEmit"
  - kind: command
    required: true
    owner: orchestrator
    detail: "cd web && pnpm run build"
  - kind: e2e
    required: true
    owner: reviewer
    detail: "Review affected UI behavior and, where runnable, capture browser evidence for in-progress status, loading, errors, and reconnection state."

### Task_4: Review, PR, Hook, Merge

- type: review
- owns:
  - none
- depends_on: [Task_2, Task_3]
- description: |
  Run independent subagent review, fix findings, create PR, run gh-review-hook until clean, and merge.
- acceptance:
  - Reviewer reports APPROVED/no blocking findings.
  - PR is created from `codex/task-42-live-meeting-page`.
  - `gh-review-hook` exits 0 after all fixes.
  - PR is merged without rewriting protected history.
- validation:
  - kind: review
    required: true
    owner: reviewer
    detail: "Independent diff review against Task 42 acceptance criteria."
  - kind: command
    required: true
    owner: orchestrator
    detail: "gh-review-hook"
  - kind: command
    required: true
    owner: orchestrator
    detail: "gh pr merge or equivalent merge command succeeds."

## Task Waves (explicit parallel dispatch sets)

- Wave 1 (parallel): [Task_1]
- Wave 2 (parallel): [Task_2]
- Wave 3 (parallel): [Task_3]
- Wave 4 (parallel): [Task_4]

## E2E / Visual Validation Spec

- provider: harness_reviewer with browser evidence if local app can run; fallback to build/typecheck plus code-level UI review if runtime environment blocks browser use.
- artifact_root: `.playwright-cli/` if Playwright/browser evidence is collected.
- base_url: local Vite preview or dev server URL.
- app_start_command: `cd web && pnpm run dev -- --host 127.0.0.1`
- readiness_check: browser can load the meeting page route through the SPA fallback.
- flows:
  - In-progress meeting route renders header/status/live transcript state.
  - Transcript stream connection/reconnecting/error states are visible and announced.
  - Permission/auth errors redirect or show panel-level errors instead of silent empty state.
- viewports:
  - desktop 1440x900
  - mobile 390x844
- evidence_requirements:
  - Reviewer notes screenshots or explicit reason browser evidence could not run.
- known_flakiness:
  - No seeded backend fixture exists yet; browser evidence may require mocked/static review if DB/auth setup is unavailable.

## Rollback / Safety

- Revert the Task 42 commit(s) to remove the SSE route and frontend live-stream connection.
- The backend route is additive and does not alter DB schema or existing transcript REST behavior.

## Progress Log (append-only)

- 2026-05-31 Wave 1 in progress: [Task_1]
  - Summary: Loaded harness skills, created branch from `origin/master`, spawned Researcher, incorporated Researcher results, checked Task 41 branch diff.
  - Validation evidence: `origin/master` matched HEAD before branch creation; `codex/task-41-live-transcription` currently has no diff from `origin/master`.
  - Notes: Repo rule suite is absent in this worktree; CI workflow is used for validation mapping.

- 2026-05-31 Wave 2 completed: [Task_2]
  - Summary: Added protected transcript SSE endpoint, changed transcript REST/SSE payload to Task 41-compatible `{ segments, status, is_final, updated_at }`, and included meeting URLs in Discord status messages when `PUBLIC_BASE_URL` is configured.
  - Validation evidence: `cargo fmt --all -- --check` pass; `cargo test status_message_tests` pass; `cargo test transcript` pass; `cargo clippy --workspace --all-targets --all-features -- -D warnings` pass; `cargo test --workspace --all-targets --all-features` pass.
  - Notes: No Task 41-only DB columns are referenced.

- 2026-05-31 Wave 3 completed: [Task_3]
  - Summary: Added frontend transcript response normalization, SSE reconnect/dedup handling, in-progress meeting notices, lifecycle status labels, summary/audio readiness handling, and dashboard refresh while live meetings are present.
  - Validation evidence: `cd web && pnpm install --frozen-lockfile` pass; `cd web && pnpm run lint` pass; `cd web && pnpm exec tsc --noEmit` pass; `cd web && pnpm run build` pass.
  - Notes: Browser evidence is assigned to Reviewer per plan; local authenticated DB fixture is not yet available.

- 2026-05-31 Wave 4 review fix in progress: [Task_4]
  - Summary: Reviewer requested fixes for discarded transcript lifecycle metadata and initial SSE auth/permission failure handling. Updated frontend to apply streamed/fetched `status`, close on `is_final`, and classify EventSource errors through `fetchTranscript` before reconnecting.
  - Validation evidence: `cd web && pnpm run lint` pass; `cd web && pnpm exec tsc --noEmit` pass; `cd web && pnpm run build` pass; `cargo fmt --all -- --check` pass; `cargo test status_message_tests` pass.
  - Notes: Reviewer re-review approved with no remaining findings.

- 2026-05-31 Final validation completed: [Task_2, Task_3, Task_4]
  - Summary: Re-ran full backend and frontend validation after review fixes.
  - Validation evidence: `cargo fmt --all -- --check` pass; `cargo clippy --workspace --all-targets --all-features -- -D warnings` pass; `cargo test --workspace --all-targets --all-features` pass; `pnpm run lint` pass; `pnpm exec tsc --noEmit` pass; `pnpm run build` pass.
  - Notes: One chained command was rerun as separate `rtk` commands and is not counted as evidence.

- 2026-05-31 gh-review-hook fixes in progress: [Task_4]
  - Summary: `gh-review-hook 63` requested 404/not_found terminal handling, stable live-status effect dependencies, server-side stream termination on final status, and shared live status definitions.
  - Validation evidence: `cargo fmt --all -- --check` pass; `pnpm run lint` pass; `pnpm exec tsc --noEmit` pass; `pnpm run build` pass.
  - Notes: Full backend checks and hook rerun pending after amend/push.

- 2026-05-31 Second gh-review-hook fixes in progress: [Task_4]
  - Summary: Fixed unsafe JSON diagnostic formatting, audio/debug availability gating, duplicated status/normalization helpers, SummaryCompleted link label, and Markdown heading spacing.
  - Validation evidence: `cargo fmt --all -- --check` pass; `cargo clippy --workspace --all-targets --all-features -- -D warnings` pass; `cargo test --workspace --all-targets --all-features` pass; `pnpm run lint` pass; `pnpm exec tsc --noEmit` pass; `pnpm run build` pass.
  - Notes: Hook rerun pending after additive commit and push.

- 2026-05-31 Third gh-review-hook fix in progress: [Task_4]
  - Summary: Added an idempotent partial index for the SSE cursor query: `(meeting_id, created_at, id) WHERE NOT is_deleted`.
  - Validation evidence: `cargo fmt --all -- --check` pass; `cargo clippy --workspace --all-targets --all-features -- -D warnings` pass; `cargo test --workspace --all-targets --all-features` pass.
  - Notes: Hook rerun pending after validation, additive commit, and push.

## Decision Log (append-only; re-plans and major discoveries)

- 2026-05-31 Decision:
  - Trigger / new insight: User corrected initial wait-cutoff behavior and instructed quality over speed.
  - Plan delta (what changed): Researcher/Reviewer/gh-review-hook waits are no longer shortened for speed; only concrete blockers or failures trigger intervention.
  - Tradeoffs considered: Slower progress is acceptable to reduce missed Task 41 dependency and review risks.
  - User approval: yes, via delegated instruction update.

- 2026-05-31 Decision:
  - Trigger / new insight: Task 41 branch exists locally but has no diff from `origin/master`.
  - Plan delta (what changed): Implement Task 42 against existing `transcripts` rows and keep SSE endpoint schema-compatible with current API.
  - Tradeoffs considered: Polling SSE is less efficient than DB notifications, but avoids inventing Task 41 persistence contracts and remains compatible when Task 41 starts writing rows.
  - User approval: yes, delegated scope allows cutting Task 41 dependencies.

- 2026-05-31 Decision:
  - Trigger / new insight: Task 41 is expected to change `/api/meetings/{meeting_id}/transcript` from an array to `{ segments, status, is_final, updated_at }` and add `transcripts.transcript_stage` / `live_chunk_id`.
  - Plan delta (what changed): Task 42 now normalizes both old and new transcript REST/SSE payload shapes on the frontend, and the Task 42 backend emits the planned response envelope without depending on new Task 41 columns.
  - Tradeoffs considered: This avoids a hard merge dependency while keeping PR integration explicit; stage/chunk metadata can be consumed after Task 41 merges.
  - User approval: yes, via delegated integration-risk update.

## Notes

- Risks:
  - SSE polling must be bounded and must not bypass meeting permissions.
  - Frontend cannot use native `EventSource` with custom abort signals; cleanup must close the connection.
  - Browser E2E may be limited by missing authenticated DB fixtures.
- Edge cases:
  - Empty transcript during recording should be "not available yet" rather than an error.
  - 401/403 stream failures should not loop forever without user-visible state.
