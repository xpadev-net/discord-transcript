# Plan: Task 44 Guild Access Controls

- status: in_progress
- generated: 2026-05-31
- last_updated: 2026-05-31
- work_type: code

## Goal
- Restrict guild dashboard data to current guild members and restrict guild settings UI/API access to guild admins.

## Definition of Done
- Dashboard APIs re-check current guild membership on every protected API request and deny stale/non-member sessions.
- Settings read and update APIs require guild admin permission.
- Frontend does not expose settings navigation or settings controls to non-admin members.
- Route/display coverage exists for admin, member, non-member/forbidden, wrong-guild, and expired-session cases.
- Required Rust/frontend validation passes.
- Independent Reviewer approves the final diff and evidence.

## Scope / Non-goals
- Scope:
  - `src/interfaces/web.rs` auth/session helpers, settings handlers, and focused tests.
  - `web/src/App.tsx`, `web/src/pages/DashboardPage.tsx`, `web/src/pages/SettingsPage.tsx`, and narrow supporting frontend files/tests.
  - PR/hook/merge lifecycle for Task 44.
- Non-goals:
  - Bot token storage, validation, or selection UI.
  - Discord permission model redesign outside member/admin gates.
  - Database schema changes.
  - Broad frontend redesign.

## Context (workspace)
- Related files/areas:
  - `src/interfaces/web.rs`
  - `web/src/App.tsx`
  - `web/src/pages/DashboardPage.tsx`
  - `web/src/pages/SettingsPage.tsx`
- Existing patterns or references:
  - Protected APIs pass through `require_auth`.
  - Meeting detail APIs already use handler-level access checks.
  - `PUT /api/guild/settings` already checks guild admin; `GET` does not.
  - Frontend API helpers centralize 401 login redirect and settings 403 mapping.
- Repo reference docs consulted:
  - `/Users/xpadev/.codex/RTK.md`
  - `$orchestration-harness`
  - `$plan-format`
  - `$engineering-quality-baselines`
  - `$git-workflow`
  - Repo rule suite: absent (`docs/coding-agent/rules/` missing).

## Open Questions
- None blocking. Decisions below record conservative defaults.

## Assumptions
- A1: Settings route/API means both `GET` and `PUT /api/guild/settings` are admin-only.
- A2: Signed sessions for a different configured guild remain authentication-invalid and return 401.
- A3: Current guild membership verification failure for a valid session should fail closed rather than allow stale dashboard/settings access.

## Tasks

### Task_1: Backend Access-Control Gates
- type: impl
- owns:
  - src/interfaces/web.rs
- depends_on: []
- description: |
  Enforce per-request guild membership for protected APIs and make settings read/update APIs require guild admin.
- acceptance:
  - Valid current guild members can reach protected APIs.
  - Non-member/left/banned users are denied and stale session permission cache is invalidated.
  - Expired/missing/invalid/wrong-guild sessions return authentication failure behavior.
  - Both settings GET and PUT require guild admin.
  - Existing meeting access behavior is preserved.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo fmt --all -- --check"
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test guild_api_tests"
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test session_reverify_tests"

### Task_2: Frontend Route And Settings UX Gates
- type: impl
- owns:
  - web/src/App.tsx
  - web/src/components/Nav.tsx
  - web/src/pages/DashboardPage.tsx
  - web/src/pages/SettingsPage.tsx
  - web/src/lib/api.ts
  - web/src/lib/types.ts
  - web/src/index.css
  - web/src/**/*.test.tsx
  - web/package.json
  - web/pnpm-lock.yaml
- depends_on: [Task_1]
- description: |
  Use current session/admin state to hide settings navigation and deny direct settings page access for non-admins.
- acceptance:
  - Admin users can see Settings navigation and settings controls.
  - Non-admin members cannot see Settings navigation.
  - Direct `/settings` access by non-admin members renders a 403-style state without settings controls.
  - Dashboard forbidden responses do not render dashboard data.
  - Expired sessions keep the existing login redirect behavior.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk pnpm --dir web run lint"
  - kind: command
    required: true
    owner: worker
    detail: "rtk pnpm --dir web exec tsc --noEmit"
  - kind: command
    required: true
    owner: worker
    detail: "rtk pnpm --dir web run build"
  - kind: command
    required: true
    owner: worker
    detail: "frontend route/display tests"

### Task_3: Full Validation, Review, PR, Hook, Merge
- type: review
- owns:
  - docs/coding-agent/plans/active/task-44-guild-access-controls-plan.md
- depends_on: [Task_1, Task_2]
- description: |
  Run full applicable validation, obtain independent Reviewer approval, create PR, run gh-review-hook until clean, and merge.
- acceptance:
  - Rust fmt/check/test/clippy coverage is run or explicitly justified.
  - Frontend lint/typecheck/build/tests pass.
  - Reviewer status is APPROVED.
  - PR is created, hook passes with exit 0, and PR is merged.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk cargo check --workspace --all-targets --all-features"
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk cargo clippy --workspace --all-targets --all-features -- -D warnings"
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk cargo test --workspace --all-targets --all-features"
  - kind: review
    required: true
    owner: reviewer
    detail: "Independent review against Task 44 acceptance criteria"

## Task Waves
- Wave 1 (parallel): [Task_1]
- Wave 2 (parallel): [Task_2]
- Wave 3 (parallel): [Task_3]

## E2E / Visual Validation Spec
- provider: unit/component tests plus reviewer source review; browser E2E only if test harness cannot cover route/display behavior.
- artifact_root: n/a unless browser E2E is added.
- base_url: n/a
- app_start_command: n/a
- readiness_check: n/a
- flows:
  - Admin session sees Settings navigation and settings form.
  - Non-admin member session does not see Settings navigation and sees 403 on direct settings route.
  - Dashboard member session renders meetings state.
  - Dashboard 403 response renders forbidden state without table data.
  - API 401 triggers login redirect behavior.
- viewports: n/a for route/display unit coverage.
- evidence_requirements: test output plus Reviewer approval.
- known_flakiness: none known.

## Rollback / Safety
- Revert this branch/PR to restore previous permissive settings read UI/API and cached membership behavior.

## Progress Log
- 2026-05-31 00:00 Wave 0 completed: research and branch safety.
  - Summary: backend and frontend Researcher agents mapped routes, helpers, tests, and risks.
  - Validation evidence: `git fetch origin --prune`; branch created from `origin/master`; no open PR for branch.
  - Notes: `z/tasks.md` is absent in this worktree; using delegation text as Task 44 source.
- 2026-05-31 00:00 Wave 1 completed: [Task_1]
  - Summary: `require_auth` now checks active guild membership on every protected API request and fails closed; settings GET/PUT now require guild admin.
  - Validation evidence: `rtk cargo fmt --all -- --check` pass; `rtk cargo test guild_api_tests` pass (7 passed); `rtk cargo test session_reverify_tests` pass (12 passed).
  - Notes: Backend route-level DB/http mocking does not exist; added focused access-control helper tests and preserved meeting access behavior.
- 2026-05-31 00:00 Wave 2 completed: [Task_2]
  - Summary: frontend now loads `/api/me`, hides Settings nav for non-admins, blocks direct `/settings`, and shows dashboard forbidden state on 403.
  - Validation evidence: `rtk pnpm --dir web run lint` pass; `rtk pnpm --dir web exec tsc --noEmit` pass; `rtk pnpm --dir web run build` pass; `rtk pnpm --dir web run test` pass (5 tests).
  - Notes: Added Vitest/jsdom/Testing Library as a minimal route/display test harness.
- 2026-05-31 00:00 Wave 3 validation completed: [Task_3]
  - Summary: full local Rust and frontend validation passed before Reviewer dispatch.
  - Validation evidence: `rtk cargo check --workspace --all-targets --all-features` pass; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings` pass; `rtk cargo test --workspace --all-targets --all-features` pass (267 passed).
  - Notes: Independent Reviewer pending.
- 2026-05-31 00:00 Reviewer follow-up completed.
  - Summary: Reviewer found settings PUT validated domain input before admin authorization. Fixed handler to check admin first and added a regression test for non-admin invalid settings payload returning 403 instead of 400.
  - Validation evidence: `rtk cargo test guild_api_tests` pass (8 passed); `rtk cargo test session_reverify_tests` pass (12 passed); `rtk pnpm --dir web run test` pass (5 passed); `rtk cargo fmt --all -- --check` pass; `rtk pnpm --dir web run lint` pass; `rtk pnpm --dir web exec tsc --noEmit` pass; `rtk pnpm --dir web run build` pass; `rtk cargo check --workspace --all-targets --all-features` pass; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings` pass; `rtk cargo test --workspace --all-targets --all-features` pass (268 passed).
  - Notes: Re-review pending.
- 2026-05-31 00:00 Independent re-review completed.
  - Summary: Reviewer status APPROVED with no findings.
  - Validation evidence: Reviewer reran `rtk cargo test guild_settings_update_checks_admin_before_domain_validation` and inspected recorded full validation.
  - Notes: Residual route-level backend test gap accepted because existing backend has no route-level DB/http mock fixture; helper-level regression coverage is in place.

## Decision Log
- 2026-05-31 00:00 Decision: Proceed without separate plan approval.
  - Trigger / new insight: User delegated end-to-end implementation, PR, hook, and merge in this worker thread.
  - Plan delta: Marked plan `in_progress`.
  - Tradeoffs considered: Waiting for explicit approval would conflict with the delegation to finish Task 44.
  - User approval: yes, via delegation request.
- 2026-05-31 00:00 Decision: Fail closed on membership re-check.
  - Trigger / new insight: Task requires per-request membership validation for dashboard APIs.
  - Plan delta: Treat current membership lookup errors as denied/upstream failure instead of allowing stale sessions.
  - Tradeoffs considered: More secure access control over lower latency and temporary Discord outage tolerance.
  - User approval: implicit in Task 44 access-control requirement.
- 2026-05-31 00:00 Decision: Settings update authorization precedes domain validation.
  - Trigger / new insight: Reviewer found non-admins could observe 400 validation errors on invalid settings update payloads before authorization.
  - Plan delta: `PUT /api/guild/settings` now checks guild admin before validating update values.
  - Tradeoffs considered: Fail-closed API behavior over early input validation diagnostics for unauthorized callers.
  - User approval: implicit in settings admin-only API requirement.

## Notes
- Risks:
  - Per-request Discord membership checks can add latency and rate-limit exposure.
  - Frontend test dependencies may need to be introduced because no route/display test harness exists.
- Edge cases:
  - Wrong-guild signed sessions stay 401 as invalid authentication context.
  - Meeting-specific anti-enumeration behavior should remain unchanged.
