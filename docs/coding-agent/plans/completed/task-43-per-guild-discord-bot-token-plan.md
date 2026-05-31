# Plan: Task 43 Per-Guild Discord Bot Token

- status: done
- generated: 2026-05-31
- last_updated: 2026-05-31
- work_type: mixed

## Goal
- Store an encrypted Discord bot token per guild, expose admin-only set/update/delete APIs and UI, and use the guild token before the global `DISCORD_TOKEN` wherever this single-guild runtime resolves Discord bot credentials.

## Definition of Done
- Guild settings can report token status without returning plaintext.
- Guild admins can validate and save a replacement token or clear it to fall back to the global token.
- Stored token material is encrypted at rest with non-secret metadata.
- Web Discord REST calls resolve the guild token first, then the global token when no guild token is configured.
- Runtime startup uses the guild token for the configured guild when present, so gateway, slash responses, recording joins, and message posts use the effective token for that guild.
- Backend and frontend validation commands pass or have explicit evidence-backed waivers.
- Independent Reviewer approves the implementation before final closeout.

## Scope / Non-goals
- Scope:
  - `migrations/**`, `src/infrastructure/sql.rs`, Rust token encryption/resolution helpers, `src/interfaces/web.rs`, `src/main.rs`, focused runtime startup wiring, frontend settings API/types/UI.
  - Token validation against Discord before persistence.
  - Admin authorization for token mutation APIs.
- Non-goals:
  - General Task 44 access-control work.
  - Multi-guild runtime supervisor or dynamic Serenity gateway token hot-swap after a token update.
  - Returning, masking, logging, or caching raw token values in frontend-visible responses.

## Context
- Repo rule suite missing: `docs/coding-agent/rules/` does not exist, so validation is derived from CI, `lefthook.yml`, and harness defaults.
- Researcher findings:
  - Current settings storage lives in `guild_settings` with nullable overrides.
  - Current web Discord REST paths use `AuthConfig.bot_token`.
  - Runtime is single guild and builds one Serenity client from `AppConfig.discord_token`.
  - Frontend settings page has no bot-token status or mutation UI.
- User approval for plan gate: waived by delegated instruction to complete Task 43 end to end in this worktree.

## Open Questions
- None blocking. Runtime token hot-swap is intentionally outside this task because the current architecture has one Serenity client bound to one token.

## Assumptions
- `GUILD_BOT_TOKEN_ENCRYPTION_KEY` will be provided anywhere per-guild tokens are set or used; no key is needed for pure global-token fallback.
- Clearing the guild token means "remove override and fall back to `DISCORD_TOKEN`."

## Tasks

### Task_1: Storage, Encryption, and Token Resolution
- type: impl
- owns:
  - `Cargo.toml`
  - `Cargo.lock`
  - `migrations/**`
  - `src/bootstrap/config.rs`
  - `src/infrastructure/**`
  - `src/main.rs`
  - `src/application/runtime.rs`
- depends_on: []
- description: |
  Add encrypted guild token persistence, token resolver helpers, config plumbing, and runtime startup token selection.
- acceptance:
  - Migration adds encrypted token fields and metadata without changing existing settings semantics.
  - Encryption helper round-trips token values and rejects tampered ciphertext.
  - Resolver returns the decrypted guild token when configured and the global token only when no guild token exists.
  - Runtime startup uses the effective token for the configured guild.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test --lib"
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo fmt --all -- --check"

### Task_2: Backend Settings API
- type: impl
- owns:
  - `src/interfaces/web.rs`
  - `src/infrastructure/**`
  - `src/main.rs`
- depends_on: [Task_1]
- description: |
  Add non-secret token status to settings, admin-only token set/delete endpoints, Discord token validation, and web Discord REST token resolution.
- acceptance:
  - `GET /api/guild/settings` returns token configured status and metadata only.
  - Token update validates the token and target guild before saving.
  - Invalid token, insufficient guild access, missing encryption key, and non-admin mutation produce explicit HTTP failures.
  - Delete removes token fields and subsequent resolution falls back to global token.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test --lib interfaces::web"
  - kind: review
    required: true
    owner: reviewer
    detail: "Reviewer checks auth, secret-handling, and Discord failure mapping."

### Task_3: Frontend Settings UI
- type: impl
- owns:
  - `web/src/lib/api.ts`
  - `web/src/lib/types.ts`
  - `web/src/pages/SettingsPage.tsx`
  - `web/src/index.css`
- depends_on: [Task_2]
- description: |
  Add token status, update, and clear controls without redisplaying raw tokens.
- acceptance:
  - UI shows configured/unconfigured status and metadata if available.
  - Token input is write-only in practice: raw token is cleared after save and never populated from API.
  - Delete is admin-only and clears status after success.
  - Existing settings save does not include token data.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk pnpm run lint"
  - kind: command
    required: true
    owner: worker
    detail: "rtk pnpm exec tsc --noEmit"
  - kind: command
    required: true
    owner: worker
    detail: "rtk pnpm run build"
  - kind: e2e
    required: true
    owner: reviewer
    detail: "Reviewer performs static/UI contract review; browser E2E may be waived if no test backend is available."

### Task_4: Full Validation, Review, PR, Hook, Merge
- type: review
- owns:
  - `docs/coding-agent/plans/active/task-43-per-guild-discord-bot-token-plan.md`
- depends_on: [Task_1, Task_2, Task_3]
- description: |
  Run full relevant validation, dispatch independent Reviewer, commit/push/PR, run gh-review-hook until clean, and merge when ready.
- acceptance:
  - Rust fmt/check/test/clippy and frontend lint/typecheck/build are run or explicitly waived with evidence.
  - Reviewer status is APPROVED or all findings are fixed and re-reviewed.
  - PR is created, hook exits 0, CI/review is clean, and PR is merged.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk cargo fmt --all -- --check"
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk cargo check --workspace --all-targets --all-features"
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk cargo test --workspace --all-targets --all-features"
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk cargo clippy --workspace --all-targets --all-features -- -D warnings"
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk pnpm run lint && rtk pnpm exec tsc --noEmit && rtk pnpm run build"
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk gh-review-hook <PR_NUMBER>"

## Task Waves
- Wave 1 (parallel): [Task_1]
- Wave 2 (parallel): [Task_2]
- Wave 3 (parallel): [Task_3]
- Wave 4 (parallel): [Task_4]

## E2E / Visual Validation Spec
- provider: static review plus optional local browser if the app can run with a test backend
- artifact_root: `.playwright-cli/` if browser validation is run
- base_url: local dev URL if available
- app_start_command: `rtk pnpm run dev -- --host 127.0.0.1`
- readiness_check: settings route loads through Vite
- flows:
  - Settings page shows token status and empty write-only token input.
  - Save token clears input and changes status.
  - Delete token clears status.
- viewports: desktop 1280px, mobile 390px if browser validation is run
- evidence_requirements: screenshot or reviewer notes confirming no raw token redisplay
- known_flakiness: authenticated backend routes may require a real OAuth session and DB, so reviewer may use static/UI contract review.

## Rollback / Safety
- Revert migration/code/UI changes; clearing per-guild token fields restores global token fallback.
- Token decrypt/validation failures fail closed instead of falling back silently.

## Progress Log
- 2026-05-31 Wave 0 completed: research
  - Summary: Repository rules missing; two Researcher subagents mapped backend/runtime and frontend settings surfaces.
  - Validation evidence: CI/lefthook-derived commands captured in this plan.
  - Notes: Current runtime supports one configured guild and one Serenity client.
- 2026-05-31 Wave 1-3 completed: [Task_1, Task_2, Task_3]
  - Summary: Added encrypted per-guild bot token storage, backend set/delete/status APIs, startup/web token resolution, and settings UI controls.
  - Validation evidence: `rtk cargo fmt --all -- --check`; `rtk cargo check --workspace --all-targets --all-features`; `rtk cargo test --workspace --all-targets --all-features`; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`; `rtk pnpm install --frozen-lockfile`; `rtk pnpm run lint`; `rtk pnpm exec tsc --noEmit`; `rtk pnpm run build`.
  - Notes: `rtk pnpm run lint` initially failed before local `node_modules` install and on this task's unformatted settings file; after installing local dependencies and formatting the touched file, lint passed.
- 2026-05-31 Reviewer pass 1 completed: CHANGES_REQUESTED
  - Summary: Reviewer found token-management endpoints could depend on the broken stored token for authorization, preventing replacement/deletion.
  - Validation evidence: Reviewer also ran `rtk git diff --check`.
  - Notes: Added global-token recovery admin check for settings/token-management endpoints and regression coverage for the retry decision.
- 2026-05-31 Post-review validation completed:
  - Summary: Re-ran full Rust and frontend checks after the recovery fix.
  - Validation evidence: `rtk cargo fmt --all -- --check`; `rtk cargo check --workspace --all-targets --all-features`; `rtk cargo test --workspace --all-targets --all-features`; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`; `rtk pnpm run lint`; `rtk pnpm exec tsc --noEmit`; `rtk pnpm run build`; `rtk git diff --check`.
  - Notes: Rust test count increased to 274 passed after adding the recovery regression.
- 2026-05-31 gh-review-hook pass 1 completed: exit 2
  - Summary: Hook found startup could fail before online recovery, settings recovery retried global auth for clean non-admins, SQL token-load semantics diverged from status semantics, key derivation needed purpose-bound KDF/normalization, and settings/token UI operations could race.
  - Validation evidence after fixes: `rtk cargo fmt --all -- --check`; `rtk cargo check --workspace --all-targets --all-features`; `rtk cargo test --workspace --all-targets --all-features`; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`; `rtk pnpm run lint`; `rtk pnpm exec tsc --noEmit`; `rtk pnpm run build`; `rtk git diff --check`.
  - Notes: Fixes were kept additive because PR #65 is open.
- 2026-05-31 gh-review-hook pass 2 completed: exit 2
  - Summary: Hook found effective bot-token resolution caused a DB query on every Discord REST call.
  - Validation evidence after fix: `rtk cargo fmt --all -- --check`; `rtk cargo check --workspace --all-targets --all-features`; `rtk cargo test --workspace --all-targets --all-features`; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`; `rtk pnpm run lint`; `rtk pnpm exec tsc --noEmit`; `rtk pnpm run build`; `rtk git diff --check`.
  - Notes: Added a WebState effective-token cache invalidated by the existing Discord cache invalidation path.
- 2026-05-31 gh-review-hook pass 3 completed: exit 2
  - Summary: Hook found `/api/me` did not use recovery-aware admin checks and the effective-token cache had no TTL.
  - Validation evidence after fix: `rtk cargo fmt --all -- --check`; `rtk cargo check --workspace --all-targets --all-features`; `rtk cargo test --workspace --all-targets --all-features`; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`; `rtk pnpm run lint`; `rtk pnpm exec tsc --noEmit`; `rtk pnpm run build`; `rtk git diff --check`.
  - Notes: `/api/me` now uses the same recovery-aware admin check and the token cache expires after 300 seconds.
- 2026-05-31 gh-review-hook pass 4 completed: exit 0
  - Summary: CI and AI review checks passed after the token cache TTL and `/api/me` recovery fixes.
  - Validation evidence: `rtk gh-review-hook 65` exited 0.
  - Notes: Ready to merge after plan lifecycle move.
- 2026-05-31 gh-review-hook pass 5 completed: exit 2
  - Summary: Hook found settings recovery retried global auth on Discord member API rate limits.
  - Validation evidence after fix: `rtk cargo fmt --all -- --check`; `rtk cargo check --workspace --all-targets --all-features`; `rtk cargo test --workspace --all-targets --all-features`; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`; `rtk pnpm run lint`; `rtk pnpm exec tsc --noEmit`; `rtk pnpm run build`.
  - Notes: Added a distinct rate-limited admin-check outcome so global-token recovery does not double-call a saturated Discord endpoint.

## Decision Log
- 2026-05-31 Decision:
  - Trigger / new insight: Runtime is single-guild and Serenity clients are token-bound.
  - Plan delta: Implement startup effective-token selection for runtime; leave dynamic gateway hot-swap outside Task 43.
  - Tradeoffs considered: A token-change supervisor would be broader and riskier than the requested settings/token path.
  - User approval: waived by explicit delegated instruction to complete Task 43 end to end.
- 2026-05-31 Decision:
  - Trigger / new insight: Reviewer identified credential-management recovery lockout.
  - Plan delta: Token settings GET/update/delete and ordinary guild settings update now retry admin authorization with the global bot token when the guild-scoped token path cannot authorize recovery.
  - Tradeoffs considered: Keeping recovery only on settings endpoints preserves normal guild-token preference for Discord calls while avoiding lockout from a bad override.
  - User approval: not requested; change is required to satisfy Task 43 delete-after-failure fallback acceptance.
- 2026-05-31 Decision:
  - Trigger / new insight: gh-review-hook found startup could block all online recovery when stored-token decryption fails.
  - Plan delta: Startup now logs stored-token resolution failures and uses the global token for runtime startup unless the failure is database-level.
  - Tradeoffs considered: Falling back at startup keeps the settings UI online; normal web Discord calls still fail closed on unusable stored tokens except for explicit settings recovery auth.
  - User approval: not requested; change is required to keep recovery paths operable.

## Notes
- Risks:
  - Missing encryption key blocks use of stored guild tokens.
  - OAuth/session membership checks still depend on a usable bot token for the configured guild.
  - Token update takes effect for web REST immediately, but the active Serenity gateway requires restart to use a newly changed token.
- Edge cases:
  - Invalid token, inaccessible guild, delete fallback, non-admin mutation, and no raw token responses.
