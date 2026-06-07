# Plan: Summary Agent Workspace Boundary

- status: draft
- generated: 2026-06-07
- last_updated: 2026-06-07
- work_type: code

## Goal
- Apply a common, minimal agent workspace boundary to all summary harnesses (`claude`, `opencode`, `cursor_agent`) so untrusted transcript summarization runs against a narrow input/output filesystem instead of the full meeting workspace.
- Move summary result collection from harness stdout to a validated output file contract shared by all harnesses.

## Definition of Done
- All CLI summary harnesses run from an agent-specific workspace that contains only approved input files, generated harness config, and an output directory.
- The app reads only the expected output artifact (for example `output/summary.md`) as the summary result.
- Stdout is no longer accepted as successful summary content; it is used only for bounded diagnostics on failure.
- Cursor-specific permissions are generated in `.cursor/cli.json` inside the agent workspace, and other harnesses use the same workspace/output contract.
- Tests cover workspace materialization, output validation, harness command construction, and failure handling.

## Scope / Non-goals
- Scope:
  - Summary and AI-memory extraction harness execution paths.
  - Shared agent workspace creation, cleanup/retention behavior, and output validation.
  - Cursor permission configuration for the generated workspace.
  - Documentation for unsafe agent harness operation.
- Non-goals:
  - Replacing Cursor/Claude/OpenCode with a non-tool LLM API.
  - Building a full FUSE filesystem in the first implementation pass.
  - Changing Discord authorization, transcript retention policy semantics, or web UI behavior.

## Context (workspace)
- Related files/areas:
  - `src/infrastructure/integrations.rs`
  - `src/application/summary.rs`
  - `src/application/ai_memory_extraction.rs`
  - `src/application/runtime.rs`
  - `src/application/worker.rs`
  - `src/infrastructure/workspace.rs`
  - `README.md`
  - `tests/**`
- Existing patterns or references:
  - `HarnessCliSummaryClient::summarize` currently returns `output.stdout`.
  - `SummaryRequest.workspace` currently points at the real meeting workspace.
  - Summary/context/transcript inputs are already materialized as files.
  - Cursor CLI supports project-level `.cursor/cli.json` permissions with `Read`, `Write`, and `Shell` tokens.
- Repo reference docs consulted:
  - `AGENTS.md` harness block
  - `docs/coding-agent/rules/*` is absent; validation selection uses codebase patterns and Rust test conventions.

## Open Questions (max 3)
- None for Task_1. The shared contract requires file outputs for both summary and AI-memory extraction, deletes per-run agent workspaces by default on success and failure, and treats Cursor permissions as defense-in-depth rather than the primary boundary.

## Assumptions
- A1: The first pass uses a normal temporary directory or workspace subdirectory, not FUSE; this is still a virtualized agent workspace because only copied approved inputs are present.
- A2: The app will continue to require `SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS=true` for agentic CLI harnesses.
- A3: The output file validator is the authoritative boundary for what is persisted and posted.

## Shared Agent I/O Contract

All CLI summary harnesses (`claude`, `opencode`, and `cursor_agent`) use the same per-run agent workspace contract. The agent process runs with the agent workspace as `current_dir`; prompts must reference only these relative paths.

### Summary run

- Required inputs:
  - `input/transcript/transcript_masked.md`: PII-masked final transcript for the meeting.
  - `input/transcript/manifest.json`: transcript metadata, including meeting IDs, language, and masking counts.
  - `input/context/manifest.json`: context manifest.
  - `input/context/speaker_roster.md`: authoritative speaker labels for the current transcript.
  - `input/context/domain_knowledge.md`: active domain knowledge.
  - `input/context/ai_memory.md`: active AI memory hints.
  - `input/context/person_aliases.md`: accepted person aliases.
  - `input/context/user_feedback.md`: accepted transcript feedback.
- Optional input:
  - `input/context/summary_template.txt`: active summary template, present only when a valid active template exists.
- Required output:
  - `output/summary.md`: the only successful summary content artifact.

The application accepts a summary run as successful only after the harness exits successfully and `output/summary.md` exists, is non-empty after trimming, is within the configured size limit, and passes markdown/content validation. Harness stdout is never accepted as successful summary markdown. Stdout and stderr are diagnostic-only and must remain bounded and sanitized in failure messages.

### AI-memory extraction run

- Required inputs:
  - `input/transcript/transcript_masked.md`: the same final transcript used for the accepted summary.
  - `input/transcript/manifest.json`: the same transcript metadata used for the accepted summary.
  - `input/summary/summary.md`: the already validated summary markdown from `output/summary.md`.
  - `input/context/manifest.json`: context manifest.
  - `input/context/speaker_roster.md`: authoritative speaker labels for the current transcript.
  - `input/context/domain_knowledge.md`: active domain knowledge.
  - `input/context/ai_memory.md`: active AI memory hints.
  - `input/context/person_aliases.md`: accepted person aliases.
  - `input/context/user_feedback.md`: accepted transcript feedback.
- Optional input:
  - `input/context/summary_template.txt`: active summary template, present only when it was materialized for the summary run.
- Required output:
  - `output/ai_memory_candidates.json`: the only successful AI-memory extraction content artifact.

The application accepts AI-memory extraction as successful only after the harness exits successfully and `output/ai_memory_candidates.json` parses as the strict extraction schema. Stdout is never accepted as successful JSON. Existing candidate validation still rejects malformed candidates, unsupported tags, wrong meeting IDs, and source excerpts that are not present in the final transcript.

### Workspace contents

The materializer may copy only the approved inputs listed above from the real meeting workspace, and may create only these generated paths inside the agent workspace:

- `input/**`: approved copied inputs for the specific run type.
- `output/`: empty before the harness starts, except for implementation-owned marker files if needed.
- `output/summary.md` or `output/ai_memory_candidates.json`: the single expected harness output for the run type.
- `.cursor/cli.json`: generated only for the `cursor_agent` harness.
- Harness-neutral metadata such as `run.json`, if owned by the application and not referenced as model input.

The materializer must not copy or symlink:

- `audio/**`, including speaker chunks, `mixdown.wav`, and `ssrc_mapping.json`.
- `debug/**`, including Whisper responses, correction prompts, summary prompts, and prior agent diagnostics.
- `summary/**` from the real meeting workspace.
- `.env`, credentials, LLM auth/config directories, hidden directories other than generated `.cursor/`, source code, Cargo files, tests, migrations, or repository metadata.
- Any unknown file discovered by directory traversal. Inputs are selected by exact path, not by copying a parent directory.

### Cursor permission intent

For `cursor_agent`, `.cursor/cli.json` is generated inside the agent workspace to express least-privilege intent: allow reads under `input/**`, allow writes only to the exact expected output file for the run, and omit or deny shell permissions. This file is defense-in-depth documentation for Cursor's project-level permission model, not the only security boundary. The primary boundary is that the agent workspace contains only approved copied files, no symlinks back to the meeting workspace, scrubbed sensitive environment variables, bounded command execution, and validated output-file collection.

### Cleanup and retention

- Success: after the expected output file is validated and its content has been persisted or handed to the next trusted application step, delete the entire per-run agent workspace, including `input/**`, `output/**`, `.cursor/`, and generated metadata.
- Failure: do not accept stdout as fallback content. Capture only bounded, sanitized diagnostics and the validation failure reason, then delete the per-run agent workspace by default.
- Future debug retention exception: if failed-run workspace retention is later enabled for operators, it must be explicit, bounded, placed under the meeting workspace's `debug/agent-runs/<run_id>/` tree, covered by retention cleanup, and must not include any files outside the allowed agent workspace contents listed above.

### Current call-path review

- `src/application/summary.rs` currently writes `transcript/transcript_masked.md` and `transcript/manifest.json`, builds prompts that reference `transcript/**` and `context/**`, and calls `summarize(..., Some(request.workspace.root()))`.
- `src/application/runtime.rs` and `src/application/worker.rs` both pass the real meeting workspace root to summary generation and optional transcript correction; summary markdown currently comes directly from the `ClaudeSummaryClient` return value.
- `src/application/ai_memory_extraction.rs` currently uses the same `summarize` interface from the real meeting workspace and parses the returned stdout string as JSON.
- `src/infrastructure/integrations.rs` currently returns successful CLI harness stdout for Claude, OpenCode, and Cursor; non-zero failures include bounded sanitized stdout/stderr diagnostics.

Task_2 through Task_5 must migrate prompts and call sites from the current `transcript/**` / `context/**` real-workspace paths to this `input/**` / `output/**` agent workspace contract.

## Tasks

### Task_1: Define Shared Agent I/O Contract
- type: design
- owns:
  - `docs/coding-agent/plans/active/summary-agent-workspace-plan.md`
  - `README.md`
- depends_on: []
- description: |
  Finalize the common contract for summary harness input files, generated config, output files, cleanup, and diagnostics. Document how the contract applies uniformly to Claude, OpenCode, and Cursor.
- acceptance:
  - Contract names exact relative paths for summary and AI-memory extraction inputs/outputs.
  - Contract states stdout is not accepted as successful summary content.
  - Contract states which files may be copied into the agent workspace and which are excluded.
  - Contract defines cleanup/retention behavior for success and failure.
  - Cursor `.cursor/cli.json` permission intent is documented without relying on it as the only boundary.
- validation:
  - kind: review
    required: true
    owner: orchestrator
    detail: "Review design against current summary, runtime, worker, and ai_memory_extraction call paths."

### Task_2: Implement Agent Workspace Materializer
- type: impl
- owns:
  - `src/infrastructure/workspace.rs`
  - `src/application/summary.rs`
  - `tests/**`
- depends_on: [Task_1]
- description: |
  Add a shared builder for per-run agent workspaces. It copies approved transcript/context inputs from the meeting workspace, creates `output/`, and writes harness-specific project config such as Cursor `.cursor/cli.json`.
- acceptance:
  - Builder creates a workspace containing only approved `input/**`, `output/`, and generated harness config.
  - Builder never symlinks back to the real meeting workspace.
  - Cursor config allows reading approved input paths and writing the expected output path.
  - Raw debug artifacts, credentials, `.env`, and real meeting debug directories are not materialized.
  - Cleanup API can remove agent workspaces after successful validation.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test workspace_layout summary --all-targets"
  - kind: review
    required: true
    owner: reviewer
    detail: "Review materializer for path traversal, symlink, and over-copy risks."

### Task_3: Change Summary Harness Contract to Output File
- type: impl
- owns:
  - `src/infrastructure/integrations.rs`
  - `src/application/summary.rs`
  - `src/application/runtime.rs`
  - `src/application/worker.rs`
  - `tests/**`
- depends_on: [Task_2]
- description: |
  Change all summary harnesses to run inside the agent workspace and produce `output/summary.md`. Read and validate that file after process exit. Treat stdout as diagnostic-only.
- acceptance:
  - Claude, OpenCode, and Cursor use the same output file contract.
  - Cursor still uses `--trust` only inside the generated agent workspace.
  - Prompt instructs the harness to write the summary to the output path and not rely on stdout.
  - Missing, empty, oversized, or invalid output file fails the summary run with sanitized diagnostics.
  - Existing summary persistence and Discord posting use validated file content only.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test asr_summary runtime_and_worker --all-targets"
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test --all-targets"
  - kind: review
    required: true
    owner: reviewer
    detail: "Review stdout-to-file contract migration for all harness branches and retry paths."

### Task_4: Apply Boundary to AI Memory Extraction
- type: impl
- owns:
  - `src/application/ai_memory_extraction.rs`
  - `src/application/runtime.rs`
  - `src/application/summary.rs`
  - `tests/**`
- depends_on: [Task_2, Task_3]
- description: |
  Run post-meeting AI memory extraction through the same isolated workspace and require `output/ai_memory_candidates.json` as the only successful extraction content artifact.
- acceptance:
  - AI memory extraction no longer runs from the real meeting workspace.
  - The extraction prompt references only approved agent workspace paths.
  - Parser reads only the validated `output/ai_memory_candidates.json` file.
  - Stdout remains diagnostic-only and is not parsed as successful JSON.
  - Candidate validation still rejects malformed or transcript-unsupported candidates.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test ai_memory_feedback --all-targets"
  - kind: review
    required: true
    owner: reviewer
    detail: "Review AI memory extraction for parity with summary harness isolation."

### Task_5: Harden Diagnostics, Cleanup, and Retention
- type: impl
- owns:
  - `src/infrastructure/integrations.rs`
  - `src/application/runtime.rs`
  - `src/application/retention_cleanup.rs`
  - `src/infrastructure/workspace.rs`
  - `tests/**`
- depends_on: [Task_3, Task_4]
- description: |
  Ensure diagnostics do not reintroduce exfiltration paths, and agent workspace cleanup is deterministic. Failed-run retention should be explicit and bounded if retained.
- acceptance:
  - Harness stdout/stderr diagnostics remain size-bounded and sanitized.
  - Agent workspace cleanup happens after successful validation and persistence.
  - Failed-run retention behavior is explicit and covered by retention cleanup if retained.
  - Error messages do not include output file contents unless sanitized and bounded.
  - Tests cover cleanup on success and failure paths.
- validation:
  - kind: command
    required: true
    owner: worker
    detail: "rtk cargo test retention_cleanup asr_summary --all-targets"
  - kind: review
    required: true
    owner: reviewer
    detail: "Review diagnostic and cleanup paths for secret/data leakage."

### Task_6: End-to-End Review and Documentation
- type: review
- owns:
  - `README.md`
  - `docs/coding-agent/plans/active/summary-agent-workspace-plan.md`
- depends_on: [Task_3, Task_4, Task_5]
- description: |
  Validate the integrated change, update operator documentation, and record final evidence.
- acceptance:
  - README explains unsafe harness opt-in, isolated workspace behavior, Cursor permissions, and output file validation.
  - Full test suite evidence is recorded.
  - Reviewer confirms all CLI harness branches share the same boundary.
  - Remaining risks are documented as follow-up items, not hidden assumptions.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: "rtk cargo test --all-targets"
  - kind: review
    required: true
    owner: reviewer
    detail: "Independent code review of shared agent workspace boundary, output validation, and docs."

## Task Waves (explicit parallel dispatch sets)

Interpretation:
- Tasks listed in the same wave are intended to be dispatched in parallel by default,
  when `owns` are disjoint and dependencies are met.
- Waves are executed sequentially.

- Wave 1 (parallel): [Task_1]
- Wave 2 (parallel): [Task_2]
- Wave 3 (parallel): [Task_3]
- Wave 4 (parallel): [Task_4, Task_5]
- Wave 5 (parallel): [Task_6]

## E2E / Visual Validation Spec

- Not applicable. This plan does not change web UI behavior.

## Rollback / Safety
- Keep `SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS=false` as the production default throughout the change.
- If file-output mode fails in production, disable unsafe agent harness rather than falling back to stdout acceptance.
- The shared agent workspace contract can be guarded by a temporary config flag during rollout, but the final target should make it the only CLI harness path.

## Progress Log (append-only)

- 2026-06-07 Draft created:
  - Summary: Planned shared agent workspace and output-file boundary for all summary harnesses.
  - Validation evidence: Static planning only.
  - Notes: Repository rule suite is absent; validation selected from Rust/test paths and security risk profile.

## Decision Log (append-only; re-plans and major discoveries)

- 2026-06-07 Decision:
  - Trigger / new insight: User clarified that all harnesses should share the same virtual filesystem/output boundary, not only Cursor.
  - Plan delta (what changed): Planned a shared materializer and output-file contract before harness-specific configuration.
  - Tradeoffs considered: Full FUSE-style virtual FS vs copied per-run agent workspace. Initial plan chooses copied workspace for lower implementation risk while preserving the security boundary.
  - User approval: pending.
- 2026-06-07 Task_1 Decision:
  - Trigger / new insight: Current summary, runtime, worker, and AI-memory extraction paths all run CLI harnesses from the real meeting workspace and accept the `summarize` return string as successful content.
  - Plan delta (what changed): Defined exact `input/**` and `output/**` relative paths for summary and AI-memory extraction, made stdout diagnostic-only, excluded real workspace directories from the agent workspace, and chose delete-by-default cleanup for both success and failure.
  - Tradeoffs considered: Keeping current `transcript/**` and `context/**` paths would reduce prompt churn but blur the boundary with the real meeting workspace. `input/**` makes copied data explicit and leaves `output/**` as the only trusted collection point.
  - User approval: delegated by Task_1 acceptance criteria.

## Notes
- Risks:
  - Cursor permissions are not a replacement for process/environment isolation.
  - Shell/network permissions remain the highest-risk escape path if granted.
  - Output validation must be strict enough that prompt-injected content cannot silently become accepted operational data.
- Edge cases:
  - Harness exits successfully but writes no output file.
  - Harness writes multiple files or writes malformed output.
  - Retry path reuses or collides with stale agent workspace.
  - AI memory extraction produces valid JSON that embeds transcript-injected instructions as candidate text.
