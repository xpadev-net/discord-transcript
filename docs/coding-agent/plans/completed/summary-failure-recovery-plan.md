# Plan: Summary Failure Recovery

- status: complete
- generated: 2026-10-14
- last_updated: 2026-10-15
- date_basis: Asia/Tokyo
- work_type: code

## Goal
- Fix the non-Whisper failures observed in osk-cluster: OpenCode Go session routing, mixdown rejected above the 512 MiB aggregate PCM limit, and PostgreSQL retry-state parameter serialization.

## Definition of Done
- Native `opencode_go` requests include a fresh, stable session ID for each summary run.
- Mixdown handles audio beyond the aggregate input cap with working memory bounded independently of total meeting audio.
- PostgreSQL retry updates serialize parameters correctly while preserving retry count and claim ownership.
- Changed Rust targets compile and the integrated changes receive independent review.

## Planner-added requirements
- Per-summary session isolation. Needed because the OpenCode Go endpoint requires a session identifier and reusing sessions could mix transcript context between meetings.
- Fixed-window mixdown processing. Needed because raising the 512 MiB cap would increase peak memory while the current deployment pod uses about 1043 MiB with no explicit memory limit.
- Align the working source snapshot with the deployed native implementation before editing. Needed because local `master` is at `fa3548a` (2026-06-24) and lacks `SummaryHarness::Native`, while the deployed `SUMMARY_HARNESS=native` / `SUMMARY_PROVIDER=opencode_go` is present in `origin/master` at `d8e0cf3` (2026-10-14).

## Scope / Non-goals
- Scope:
  - Start a task branch from `origin/master` to match the deployed native harness source.
  - Add session routing to native OpenCode Go requests in `src/infrastructure/agent.rs`.
  - Replace whole-meeting PCM retention for mixdown with bounded window processing.
  - Fix retry parameter binding in the SQL queue.
- Non-goals:
  - Whisper connectivity or transcription failures.
  - Cluster mutations, production job replay, or database schema changes.
  - Adding or running tests; validation is compile-only plus independent source review.

## Design
- Chosen: Work from a new local task branch based on the deployed-source snapshot `origin/master` without changing the existing `master` branch; give each native summary run a generated session ID and set the OpenCode Go request header through the Rig HTTP client; stream mixdown in fixed windows with file-backed output/resampling; cast the retry count through text before the SQL integer cast. Structure: keep provider/session logic in the native agent, audio logic in the mixdown path, and SQL typing at the retry query. Evolution: no database-wide binding abstraction or new service. Verification: compile changed targets and independently review request construction, bounded buffers, and placeholder/type alignment. Operation: session creation is local; audio uses bounded heap and more disk I/O; retry lifecycle stays the same. Human: session IDs and phase errors remain diagnosable. Safety: no cross-meeting context reuse; temp audio stays inside meeting workspace and is removed on failure.
- Alternative: Stay on the June checkout and port native support from `origin/master`; keep whole-meeting audio and raise the limit; replace string SQL parameters with a general typed `ToSql` enum. Structure: broad native runtime/config backport, smaller audio edit, and cross-cutting SQL changes. Evolution: risks duplicating newer work, retains large meeting allocations, and introduces a generic parameter API. Verification: wider backport needs more confidence; simpler audio validation; all SQL callers need type review. Operation: less disk I/O but larger memory peaks and broader churn. Human: confusing divergence from deployed behavior. Safety: larger merge risk, OOM risk, and wider serialization surface.
- Why chosen: Matching `origin/master` avoids fixing the wrong harness version; session headers answer the provider's explicit error, fixed-window processing avoids raising heap pressure, and a narrow SQL cast fixes the known string/integer bind mismatch.

## Compatibility stance
- surface: Native OpenCode Go HTTP request header contract.
- stance: preserve
- justification: The deployed provider returned `MissingSessionID` and explicitly requires `x-opencode-session`; the locatable consumer is the configured `opencode_go` endpoint. The fix adds the required per-run metadata without changing the request body or persisted formats.

## Context (workspace)
- Related files/areas in `origin/master`:
  - `src/infrastructure/agent.rs`
  - `src/audio/meeting_audio.rs`
  - `src/application/runtime.rs`
  - `src/infrastructure/sql_store.rs`
  - `src/infrastructure/sql.rs`
- Existing patterns or references:
  - `origin/master` contains the deployed native agent/provider implementation; local branch source does not.
  - `docs/coding-agent/plans/active/summary-agent-workspace-plan.md` defines the isolated agent workspace and output contract.
  - OpenCode Go's service error requires `x-opencode-session`; OpenCode documents a stable session identifier per conversation: https://docs.opencode.ai/docs/go/.
  - `docs/coding-agent/rules/{index,common,orchestrator}.md` are absent; lessons log consulted.
- Design record consulted and deviations from its acceptance:
  - Summary Agent Workspace Boundary plan; no change to that contract is intended.

## Open Questions (max 3)
- None. User selected the live native implementation as the target.

## Assumptions
- A1: `SqlJobQueue::retry` binds a retry-count string against an integer SQL parameter — source: `origin/master:src/infrastructure/sql_store.rs` and `origin/master:src/infrastructure/sql.rs`; confirm during implementation.
- A2: The 512 MiB guard in `origin/master:src/audio/meeting_audio.rs` rejects the 538,129,632-byte input shown in logs.
- A3: Native OpenCode Go uses Rig's OpenAI-compatible client without the required session header — source: `origin/master:src/infrastructure/agent.rs`; confirm during implementation.

## Tasks

### Task_1: Align task source with deployment
- type: chore
- owns:
  - Git branch metadata and tracked workspace files
- depends_on: []
- description: |
  Create a task branch from `origin/master` so implementation targets the native source used by the deployment. Preserve the existing local `master` branch and untracked user files.
- acceptance:
  - The task branch starts at `origin/master` (`d8e0cf3` at plan time).
  - Existing `master` remains unchanged.
  - Existing untracked `docs/codebase-review-2026-07-19.md` remains untouched.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "Verify branch, HEAD, and worktree status before edits."

### Task_2: Add OpenCode Go session routing
- type: impl
- owns:
  - `src/infrastructure/agent.rs`
  - `Cargo.toml`
  - `Cargo.lock`
- depends_on: [Task_1]
- description: |
  Generate one session ID per summary run and attach it as `x-opencode-session` to OpenCode Go provider requests, preserving a stable ID across that run's tool-call turns.
- acceptance:
  - Every OpenCode Go completion request includes the same non-empty session ID within one summary run.
  - Different summary runs receive different IDs.
  - Other providers are not sent the OpenCode-specific header.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "Compile changed Rust targets; do not run tests."
  - kind: review
    required: true
    owner: reviewer
    detail: "Verify header propagation, ID lifetime, and provider scoping."

### Task_3: Bound mixdown working memory
- type: impl
- owns:
  - `src/audio/meeting_audio.rs`
  - `src/application/runtime.rs`
- depends_on: [Task_1]
- description: |
  Replace aggregate whole-meeting PCM retention for mixdown with fixed-window mixing and bounded file-backed resampling/WAV output. Retain per-file, duration, and corrupt-chunk protections.
- acceptance:
  - Valid audio exceeding the current 512 MiB aggregate input limit can be merged.
  - Peak working memory is bounded by window size, one source chunk, and fixed output buffers rather than total meeting PCM.
  - Temporary files stay within the meeting workspace and are cleaned on failure.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "Compile changed Rust targets; do not run tests."
  - kind: review
    required: true
    owner: reviewer
    detail: "Check buffer bounds across load, mix, resample, write, and failure cleanup paths."

### Task_4: Correct PostgreSQL retry parameter binding
- type: impl
- owns:
  - `src/infrastructure/sql_store.rs`
  - `src/infrastructure/sql.rs`
- depends_on: [Task_1]
- description: |
  Confirm the retry query bind mismatch and align parameter serialization with SQL types without changing claim-token ownership semantics or the shared executor contract unnecessarily.
- acceptance:
  - Summary job retry updates no longer fail parameter serialization.
  - Retry count, error message, job status, and claim-token checks retain their intended semantics.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "Compile changed Rust targets; do not run tests."
  - kind: review
    required: true
    owner: reviewer
    detail: "Verify SQL placeholders, Rust bind types, and retry ownership behavior."

### Task_5: Integrated review
- type: review
- owns: []
- depends_on: [Task_2, Task_3, Task_4]
- description: |
  Independently review the integrated changes against the three observed failures and confirm Whisper paths were not changed.
- acceptance:
  - Reviewer status is APPROVED.
  - Compile evidence and remaining risks are recorded.
- validation:
  - kind: review
    required: true
    owner: reviewer
    detail: "Review all fixes and evidence against acceptance criteria."

## Task Waves (explicit parallel dispatch sets)
- Wave 1 (parallel): [Task_1]
- Wave 2 (parallel): [Task_2, Task_3, Task_4]
- Wave 3 (parallel): [Task_5]

## Rollback / Safety
- Keep the existing local `master` branch and user untracked files intact.
- Do not mutate cluster resources or replay production jobs.
- Keep mixdown temporary files inside the meeting workspace and clean them on failure.

## Progress Log (append-only)
- 2026-10-15 GitHub review follow-up:
  - Summary: GitHub Codex review identified a regression where a valid but far-future chunk caused manifest scanning to reject the entire mixdown before the streaming cluster builder could skip or truncate it. Removed the redundant manifest-wide span rejection; downstream wall-clock cap remains responsible for placement and truncation.
  - Validation evidence: Existing regression coverage `mixdown_skips_chunks_beyond_meeting_wall_clock_cap` exercises this behavior; local tests were not run. Formatting, compile, CI, and fresh Codex review are pending.
  - Notes: Unresolved thread will be replied to and resolved after the fix is pushed.
- 2026-10-15 PR review follow-up:
  - Summary: `codex exec review` found the existing formatter difference, which was formatted and pushed. Subsequent review identified resampling window phase concerns and source buffer retention; output slicing now uses cluster-local global output indices with an asserted modulo-three read alignment, and the source buffer is reused. Codex re-review confirmed no phase mismatch and confirmed final output counts match the existing resampler for non-multiple-of-three spans.
  - Validation evidence: `cargo fmt --all -- --check` and `cargo check --locked` passed after the follow-ups; Codex targeted re-review returned no actionable finding on the phase/output-count concerns.
  - Notes: Tests remain unrun. PR CI is being rerun against the current head.
- 2026-10-15 CI follow-up:
  - Summary: PR Clippy found dead legacy mixdown writer helpers, a `let_and_return` pattern, and manual ceiling division. Removed the superseded writer helpers and applied the standard return and `div_ceil` forms.
  - Validation evidence: `cargo fmt --all -- --check`, `cargo check --locked`, and `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` all passed; latest `codex exec review --base master` found no actionable regressions.
  - Notes: Test execution is left to the PR CI workflow; no tests were run locally.
- 2026-10-15 Closeout:
  - Summary: Completed Tasks 2–4 and received APPROVED integrated review for Task_5. Changes address OpenCode Go session routing, bounded mixdown for meetings beyond 512 MiB aggregate PCM, and retry SQL parameter serialization; Whisper paths were not changed.
  - Validation evidence: `rtk proxy cargo check --locked` passed without warnings; `git diff --check` passed; plan validator passed in balanced mode; independent source review approved. Tests were not run as instructed.
  - Notes: No production cluster mutation, job replay, commit, or push was performed. The prior untracked codebase review remains untouched. No ADR proposed: these changes implement scoped fixes within the existing provider, audio, and SQL boundaries.
- 2026-10-15 Implementation started:
  - Summary: User approved the reviewed plan. Created `codex/summary-failure-recovery` at `d8e0cf3` (`origin/master`); existing `master` and untracked review file remain untouched. Dispatched Tasks 2–4 in parallel with isolated file ownership.
  - Validation evidence: `git rev-parse HEAD` and `git rev-parse origin/master` both returned `d8e0cf3bddfb2f07bc3a45a9a4dde70f80fb5a80`; working tree showed only the pre-existing untracked review file before implementation.
  - Notes: GitHub PR lookup could not connect to `api.github.com`; branch creation did not rewrite or mutate existing branch history.
- 2026-10-15 Compile feedback:
  - Summary: The initial compile found that `rig_core::http_client` does not re-export its Reqwest transport; the configured transport lives in the separate `rig-reqwest` crate.
  - Validation evidence: `cargo check --locked` reached the crate and failed on unresolved import `rig_core::http_client::ReqwestClient`.
  - Notes: Expanded Task_2 owns to include the direct transport dependency manifest/lock entries; header behavior remains in the provider-specific agent module.
  - Resolution: Added the direct `rig-reqwest` dependency and constructed its `ReqwestClient` from `rig_reqwest::reqwest::Client`, matching the transport's reqwest 0.13 version. The next compile passed but identified six production dead-code warnings from superseded whole-cluster helpers; Task_3 follow-up will retain them only for existing test helpers.
  - Validation: `rtk proxy cargo check --locked` passed after the follow-ups with no warnings; `validate_plan.py --mode balanced` passed. Tests were not run.
- 2026-10-14 Draft created:
  - Summary: Scoped the three non-Whisper failures and corrected the source target to match the live native harness.
  - Validation evidence: Plan validator pending; implementation has not started.
  - Notes: Local `master` predates native harness; user selected the live implementation. Repository rules absent; pre-existing untracked codebase review is untouched.

## Decision Log (append-only; re-plans and major discoveries)
- 2026-10-14 Decision:
  - Trigger / new insight: Cluster env is `SUMMARY_HARNESS=native` and `SUMMARY_PROVIDER=opencode_go`; local branch only supports CLI harnesses, while `origin/master` at `d8e0cf3` contains native agent code.
  - Plan delta (what changed): Target native OpenCode Go request routing and add a non-destructive task branch alignment before implementation.
  - Tradeoffs considered: Porting native support onto the stale branch risks duplicating newer architecture; a task branch from `origin/master` matches the deployed runtime.
  - User approval: yes for the live native implementation target; plan approval later recorded after reviewer approval and user confirmation.
  - Record proposed: none
