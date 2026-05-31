# Lessons Log (Coding Agent)

Purpose:
- capture recurring mistakes and the prevention mechanism
- enable "read once, don't repeat" improvements

## How to use

- Append a new entry after any user correction or significant miss.
- Keep entries short and actionable.
- Promote repeated/high-severity lessons into repo rules, harness migration candidates, troubleshooting notes, or accepted residual-risk records.

## Tags (recommended)

- planning
- validation
- delegation
- review
- ui-e2e
- tooling
- ci
- scope-owns

## Entries

## 2026-05-31 - Index Polling Cursors  [tags: review, performance]

Context:

- Plan: docs/coding-agent/plans/active/task-42-live-meeting-page-plan.md
- Task/Wave: gh-review-hook final finding
- Roles involved: Orchestrator | AI reviewer

Symptom:

- `gh-review-hook` found that the SSE transcript polling query ordered by `(created_at, id)` without a matching index.

Root cause:

- The implementation added a polling cursor query but did not add a persistence-level performance check for the new ordering path.

Fix applied:

- Added an idempotent partial index on `(meeting_id, created_at, id) WHERE NOT is_deleted` and included it in runtime migrations.

Prevention:

- Repo rule candidate:
  - audience: reviewer
  - proposed rule: Any polling query introduced for live UI must be checked for an index that matches its filter and ordering keys.
- Residual risk / waiver:
  - none

Evidence:

- Pending final validation and hook rerun after the migration/index commit.

## 2026-05-31 - Keep Hook Fixes Minimal But Complete  [tags: review, validation]

Context:

- Plan: docs/coding-agent/plans/active/task-42-live-meeting-page-plan.md
- Task/Wave: gh-review-hook rerun
- Roles involved: Orchestrator | AI reviewer

Symptom:

- A second `gh-review-hook` pass found smaller consistency issues: unsafe JSON diagnostic formatting, audio rendering before availability, duplicate status helpers, duplicate transcript response normalization, and Markdown heading spacing.

Root cause:

- Earlier fixes addressed the major reconnect loop but left adjacent consistency and documentation-lint details for the hook to catch.

Fix applied:

- Shared status helpers and transcript response normalization were centralized, audio/debug rendering was limited to `posted`, SSE encode errors now use `serde_json`, SummaryCompleted link text now says "詳細ページ", and Markdown headings were normalized.

Prevention:

- Repo rule candidate:
  - audience: common
  - proposed rule: When review fixes introduce a shared concept, scan for existing duplicate helpers in adjacent files before rerunning external review.
- Residual risk / waiver:
  - none

Evidence:

- After fixes, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-targets --all-features`, `pnpm run lint`, `pnpm exec tsc --noEmit`, and `pnpm run build` passed.

## 2026-05-31 - Never Amend Open PR Branches  [tags: tooling, ci]

Context:
- Plan: docs/coding-agent/plans/active/task-42-live-meeting-page-plan.md
- Task/Wave: gh-review-hook fix commit
- Roles involved: Orchestrator

Symptom:
- After PR creation, the Orchestrator used `git commit --amend` on the PR branch.

Root cause:
- The Orchestrator treated review-hook fixes like pre-PR cleanup and failed to re-check the open-PR no-history-rewrite rule before amending.

Fix applied:
- No force push was performed. The amended state was saved to a backup branch, the PR branch was restored to `origin/codex/task-42-live-meeting-page`, and the hook fixes were reapplied as a normal follow-up commit.

Prevention:
- Dispatch/plan guardrail:
  - After PR creation, every fix must be a new normal commit; do not use amend/rebase/squash on the PR branch.
- Repo rule candidate:
  - audience: common
  - proposed rule: Once a branch has an open PR, all review fixes must be additive commits unless the user explicitly authorizes history rewriting.
- Residual risk / waiver:
  - none

Evidence:
- Backup branch `codex/task-42-live-meeting-page-amend-backup` was created before restoring the PR branch and applying the hook-fix patch as an additive commit.

## 2026-05-31 - Treat 404 As Terminal In Live Streams  [tags: review, validation]

Context:
- Plan: docs/coding-agent/plans/active/task-42-live-meeting-page-plan.md
- Task/Wave: gh-review-hook
- Roles involved: Orchestrator | AI reviewer

Symptom:
- `gh-review-hook` found that a deleted meeting could leave the live transcript UI retrying forever because 404/not_found was not terminal.
- It also found stream restarts on every live-to-live status change and server-side SSE polling after final status.

Root cause:
- The reconnect design classified only 403 as terminal and used the raw status value as an effect dependency instead of the live/non-live category.

Fix applied:
- The frontend now treats 404/not_found as terminal, keys the SSE effect on the live/non-live boolean, and shares live status definitions from `web/src/lib/meetingStatus.ts`.
- The backend now ends the SSE stream after emitting a final transcript envelope.

Prevention:
- Repo rule candidate:
  - audience: reviewer
  - proposed rule: Live-stream retry review must classify terminal resource states such as 403 and 404, not only transient network failures.
- Dispatch/plan guardrail:
  - For React stream effects, depend on stable lifecycle categories when live-to-live transitions should not restart the transport.
- Residual risk / waiver:
  - none

Evidence:
- `gh-review-hook 63` exited 2 with the findings; fixes were applied and frontend lint/typecheck/build plus Rust format passed before rerunning wider checks.

## 2026-05-31 - Do Not Chain Validation Commands Under RTK Rule  [tags: tooling, validation]

Context:
- Plan: docs/coding-agent/plans/active/task-42-live-meeting-page-plan.md
- Task/Wave: final validation
- Roles involved: Orchestrator

Symptom:
- The Orchestrator ran a chained web validation command where only the first command was prefixed with `rtk`.

Root cause:
- The Orchestrator optimized for parallel validation and forgot the repository's stricter "every shell command uses `rtk`" rule applies to each command in a shell chain.

Fix applied:
- The chained command was not counted as validation evidence; each web check was rerun as a separate `rtk` command.

Prevention:
- Dispatch/plan guardrail:
  - In this repo, do not chain validation commands with `&&`; run each validation as its own `rtk` command.
- Repo rule candidate:
  - audience: common
  - proposed rule: Avoid shell command chains in this repository because every individual command must be visibly prefixed with `rtk`.
- Residual risk / waiver:
  - none

Evidence:
- Reran `rtk pnpm run lint`, `rtk pnpm exec tsc --noEmit`, and `rtk pnpm run build` separately after the mistake.

## 2026-05-31 - Consume Stream Lifecycle Fields  [tags: review, ui-e2e]

Context:
- Plan: docs/coding-agent/plans/active/task-42-live-meeting-page-plan.md
- Task/Wave: Task_4 reviewer pass
- Roles involved: Orchestrator | Reviewer

Symptom:
- Reviewer found that the frontend parsed Task 41-compatible transcript envelopes but discarded `status` and `is_final`.
- Reviewer also found initial SSE handshake 401/403 failures would reconnect indefinitely because native `EventSource` does not expose HTTP status.

Root cause:
- The implementation focused on segment diff merging and did not treat stream metadata and handshake errors as acceptance-critical lifecycle inputs.

Fix applied:
- The frontend now applies streamed/fetched transcript status to `meeting.status`, closes the stream on `is_final`, and verifies access with `fetchTranscript` before reconnecting after an EventSource error.

Prevention:
- Dispatch/plan guardrail:
  - For streamed UI APIs, review must verify both payload data and lifecycle metadata drive page state transitions.
- Repo rule candidate:
  - audience: reviewer
  - proposed rule: SSE/EventSource review must include initial handshake auth failures because `EventSource.onerror` cannot inspect HTTP status codes.
- Residual risk / waiver:
  - Browser evidence remains limited until authenticated local fixtures or Playwright tooling are available.

Evidence:
- Reviewer `019e7d54-d0dc-7e13-ae00-181beabe7918` reported CHANGES_REQUESTED; fixes were applied and `pnpm run lint`, `pnpm exec tsc --noEmit`, `pnpm run build`, `cargo fmt --all -- --check`, and `cargo test status_message_tests` passed.

## 2026-05-31 - Do Not Rush Delegated Research  [tags: planning, delegation]

Context:
- Plan: docs/coding-agent/plans/active/task-42-live-meeting-page-plan.md
- Task/Wave: Task_1
- Roles involved: Orchestrator | Researcher

Symptom:
- The Orchestrator initially decided to stop waiting for a Researcher after a short timeout and proceed with minimal local exploration.
- The user corrected the workflow to prioritize quality over speed for this task.

Root cause:
- The Orchestrator over-weighted the prior instruction to avoid waiting too long and under-weighted the newer quality bar for Task 42.

Fix applied:
- Treat the cutoff decision as withdrawn, incorporate the completed Researcher findings into planning, and allow sufficient wait time for design, review, and gh-review-hook feedback.

Prevention:
- Dispatch/plan guardrail:
  - For this task, do not shorten Researcher/Reviewer waits for speed unless a concrete blocker, branch safety issue, validation failure, gh-review-hook finding, or PR/merge coordination issue appears.
- Repo rule candidate:
  - audience: orchestrator
  - proposed rule: When newer user direction raises the quality bar, update the active plan and do not continue executing under an older speed-optimization assumption.
- Harness migration candidate:
  - category: delegation
  - proposed_home: orchestration-harness/references/async-dispatch-lifecycle.md
  - generalized_rule: User corrections that reverse delegation wait policy should be reflected in active plan assumptions before further implementation.
  - suggested_change: Add a short "latest user directive wins for wait policy" reminder to async dispatch guidance.
- Residual risk / waiver:
  - none

Evidence:
- User correction received on 2026-05-31 in the Task 42 delegated thread.
